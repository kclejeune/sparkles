# Sparkles

A high-performance RDF / SPARQL / OWL database in Rust. It aims to be a
**functional re-implementation of [Apache Jena](https://jena.apache.org/) + Fuseki**
(same protocols, same semantics, same operational model), built on the
**index and execution architecture of [QLever](https://github.com/ad-freiburg/qlever)**.
It also ships a SvelteKit UI for database management, graph visualization and
interactive querying.

* `docs/AUDIT.md` covers the Jena and QLever audits and the language decision (Rust vs. Go).
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
| `crates/sparkles-server` | axum HTTP server + `sparkles` CLI | jena-fuseki2, jena-cmds |
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
| Parallel bulk loader (Turtle / N-Triples / N-Quads / TriG / RDF/XML / JSON-LD, `.gz`) | ✅ |
| External sort for inputs larger than the memory budget | ✅ |
| Planner statistics (per predicate counts, distinct S/O, classes, graphs) | ✅ |
| MVCC snapshots, single writer (MR+SW), WAL with crash-safe replay | ✅ |
| Durable commit ids: dataset UUID, gap-free commit sequence with timestamps and net counts, receipts on writes, `Sparkles-Commit` headers, commit catalog (`/$/commits`, `sparkles log`) | ✅ |
| Point-in-time reads (`?at=commit:N`, `time:…`, `snapshot:NAME` on queries, explain and Graph Store GET, with Memento headers) and named snapshots that keep a commit readable across compaction; optional retention window (`/$/snapshots`, `/$/history`, `sparkles snapshot`, `query --at`, `dump --at`) | ✅ |
| Compaction into a new generation (`gen-NNNN`, atomic `CURRENT` switch) | ✅ |
| Backups (gzipped N-Quads) | ✅ |
| Read-only integrity check (`sparkles check`, `sparkles::check`): layout, every block of the 7 permutations, cross-permutation consistency, vocabulary order and id ranges, WAL checksums and commit continuity, catalog, full-text segment checksums; safe next to a running server | ✅ |
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
| SERVICE (federated query, SILENT) | ✅ |
| Vector similarity: `spk:vector` literals, `spk:cosine`/`dot`/`euclidean`, exact top-k `spk:vectorSearch` scoped to the active graph (no approximate / HNSW index yet) | ✅ |
| Full-text search: Jena `text:query` subset, BM25 via Tantivy, per-quad documents kept current in each commit, graph-scoped top-k (`text` cargo feature, on in the server) | ✅ |
| Results: JSON, XML, CSV, TSV, `x-sparkles+json` (with executed plan); RDF: Turtle, N-Triples, N-Quads, TriG, JSON-LD, RDF/XML | ✅ |
| W3C conformance: SPARQL 1.1 query **328/328**, SPARQL 1.1 update **157/157**, SPARQL 1.0 **479/482**, SPARQL 1.2 **265/269** (all 7 failures are `spargebra` parser limitations, see `tests/w3c-known-failures.txt`) | ✅ |

### Server (Fuseki equivalent), reasoning, validation, UI

| Feature | Status |
|---|---|
| SPARQL protocol, GSP, upload, `/$/` admin (datasets, stats, compact, backup, tasks), Jena special graphs (`urn:x-arq:DefaultGraph`/`UnionGraph`) | ✅ |
| Jena-style CLI (`load`, `query`, `update`, `dump`, `compact`, `backup`, `stats`, `infer`, `shacl`, `schema`, `clone`, `check`), operating on the database directory directly | ✅ |
| Clone a dataset into an independent sandbox from one snapshot (`POST /$/datasets/{ds}/clone`, `sparkles clone`): same quads and blank-node ids, new dataset id with `forkedFrom`, inferences copied or dropped | ✅ |
| Embedded Rust API (`sparkles::Dataset`) and fluent query builder (`sparkles::querybuilder`) | ✅ |
| RDFS / OWL 2 RL materialization, Jena rule syntax (`sparkles-reasoner`, `/$/reason`, `sparkles infer`) | ✅ |
| Inference freshness: the commit inferences were made at, `stale` / `commitsSince` in `GET /$/reason/{ds}`, dataset info, `/$/stats` and a `Sparkles-Inferences` header; re-run of the recorded profile; opt-in automatic re-runs (`serve --auto-reason`) | ✅ |
| Inconsistency diagnostics: 7 checks from the OWL 2 RL rules with a `false` conclusion (`owl:Nothing` members, disjoint classes, sameAs/differentFrom, functional-property literals, …), `GET /$/reason/{ds}/diagnostics`, `sparkles infer --check`; a subset, never a consistency proof | ✅ |
| SHACL Core + SHACL-SPARQL validation (`sparkles-shacl`): W3C suite **98/98** Core, **20/20** SPARQL; parallel, index-backed | ✅ |
| Write-time SHACL validation: every commit's post-state is validated before anything is written (`reject` refuses with `422`, `warn` commits and reports), shapes from dataset graphs or a file, relevance skip, fail-closed without a guard (`/$/validation/{ds}`, `sparkles validation`, `--no-validate`); full validation per write (incremental validation is planned) | ✅ |
| Fuseki `/{ds}/shacl` endpoint (`graph=default\|union\|<iri>`, report as Turtle / N-Triples / JSON-LD / JSON, validates data ∪ inferences) and `sparkles shacl` command | ✅ |
| Query result cache controls: `--result-cache-mb`, `nocache=true`, cache stats in `/$/stats`, `POST /$/cache/clear/{ds}` | ✅ |
| Schema discovery (`GET /$/schema/{ds}`, `sparkles schema`, `sparkles::schema`): classes and predicates with exact per-graph counts (triples, distinct subjects/objects, object kinds, datatypes, languages, max objects per subject) kept apart from their RDFS/OWL declarations; subClassOf roots and cycles; cursor pagination bound to one snapshot; time and entry budgets that fail instead of truncating | ✅ |
| MCP server for LLM agents (`sparkles mcp`, stdio; `mcp` cargo feature, on by default): list datasets, describe the schema, run bounded SPARQL (compact table or JSON, truncation announced with the exact total), explain with warnings, describe a resource, list commits, full-text and vector similarity search; `atCommit` keeps several calls on one snapshot; engine budgets on every call, SERVICE off, no writes; MCP revisions `2026-07-28`, `2025-11-25` and `2025-06-18` | ✅ |
| Observability: `X-Request-Id`, one structured access-log line per request (text or JSON), Prometheus `/$/metrics`, readiness `/$/ready`, graceful drain on SIGTERM | ✅ |
| Per-query budgets (estimated intermediate-result memory, response size, rows) failing with `507`; queries stop when their client disconnects | ✅ |
| OpenTelemetry (`otel` cargo feature, off at run time unless `--otel` or `OTEL_*` enable it): OTLP traces with W3C `traceparent` in and out (SERVICE, LOAD), HTTP/database semantic-convention attributes, query phase and operator-tree spans synthesized from recorded timings, commit and background-task spans; metrics (`http.server.request.duration` plus the Prometheus registry, bridged); optional OTLP logs with trace correlation | ✅ |
| Rate limiting: per-client GCRA buckets and concurrency caps per request class (`auth`, `query`, `update`, `admin`) with per-dataset overrides, trusted-proxy client addresses, `429`/`503` with `Retry-After` and `RateLimit` headers, bounded client tracking, SIGHUP reload; off by default | ✅ |
| Authentication and per-dataset access control (`serve --auth-config`, off by default): levels `read` < `write` < `admin` by dataset name or pattern plus `metrics` / `federate` / `server-admin`, deny by default, hidden datasets answer `404`; HTTP Basic users (argon2id), scoped, expiring, revocable API tokens (`Authorization: Bearer spk_…`, hashed at rest, never above their owner), OIDC sign-in for the UI (native, authorization code + PKCE), trusted forward-auth proxy headers from configured CIDRs or a Unix socket, group-to-role mapping; CSRF and CORS rules for cookies; `sparkles auth login` (browser loopback or device code) and remote `query` / `update` / `load --server`; see [docs/API.md](docs/API.md#authentication-and-access-control) | ✅ |
| SvelteKit UI: datasets, query editor, results table/graph/plan, explorer, server page with readiness, request and cache panels, schema browser on `/$/schema` (graph selection, inference toggle, observed counts and object kinds next to declarations), commit history and write receipts, full-text search (index admin panel, ranked `text:query` search in Explore), vector similarity ("Similar" in the explorer, compact vector literals); embedded in the server binary; Vitest unit tests, and a mock server for UI development | ✅ |

## Performance

See [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md). It compares Jena/Fuseki, QLever and
Fluree using hyperfine over HTTP, with every result cache off and each engine measured
on its own. Before timing, it checks that all engines return the same answers.

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.6 s** (Fluree 1.4, QLever 1.5, TDB2 4.3) | **4.8 s** (QLever 9.1, Fluree 10.2, TDB2 42.6) |
| Fastest of the four | 18 of 20 queries | 15 of 20 queries |
| Loses to QLever | none | `range-topk` 1.8×; `minus`, `path-plus` ≈ |
| Loses to Fluree | `distinct-obj` 1.5×, `two-hop-count` ≈ | `contains` 1.6×, `distinct-obj` 1.6× |
| vs. Fuseki | 1.6–35× faster; Fuseki errors on `foaf:knows*` | 2.7–690× faster (`path-plus` ≈) |
| Update latency (1 triple, real insert) | **5.1 ms** (Fluree 6.5, QLever 11.8, Fuseki 41.7) | 7.6 ms (**Fluree 6.8**, QLever 15.8, Fuseki 38.6) |
| Throughput, 16 clients | **940 q/s** (Fluree 497, QLever 408, Fuseki 53) | **191 q/s** (QLever 57, Fluree 51, Fuseki 7) |
| Server memory | 364 MiB (**QLever 225**, Fuseki 1.7 GiB, Fluree 2.2 GiB) | 897 MiB (**QLever 362 MiB**, Fluree 3.1 GiB, Fuseki 3.9 GiB) |

The Sparkles column was re-measured after the latest executor and allocator changes;
the other engines' numbers are from the earlier run on the same machine and data.
Against QLever at 10.5M, Sparkles wins 17 of 20 queries, several by 7–24×
(`distinct-obj`, `contains`, `regex-iri`, `lang-filter`, `knows-reach`, `count-all`).
Fluree is 1.5–60× slower than Sparkles on general joins, OPTIONAL, subqueries, grouping,
sorting and path traversal, and was OOM-killed (26 GB) on `optional-chain` at 10.5M.

Where Sparkles still loses on performance:
* **Range filters with ORDER BY … LIMIT:** QLever is 1.8× faster on `range-topk` at
  10.5M. Sparkles reads only the matching id ranges of inline numbers, but must still
  test the non-canonical numerals (vocabulary literals) and decode the surviving rows.
* **Single-predicate scans:** Fluree is 1.6× faster at 10.5M on `contains` and
  `distinct-obj`.
* **Update latency:** Fluree commits slightly faster at 10.5M (it indexes in the
  background).
* **Memory:** Sparkles materializes every intermediate result and buffers whole
  responses, and it keeps a decoded-block cache (about 450 MiB of the 897 MiB at 10.5M).
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
| Spatial | GeoSPARQL (`geof:` functions, spatial index) | ✗ none |
| Shape languages | ShEx (jena-shex) | ✗ SHACL only |
| Inference | on-the-fly `InfModel`, backward / hybrid rules (LP engine), OWL Micro/Mini/Full | forward materialization only (RDFS, OWL 2 RL subset, Jena forward rules); not maintained incrementally: after updates the inferences are reported stale and re-run on request or, opt-in, automatically (a full recomputation); inconsistency detection covers a fixed subset of the OWL 2 RL `false` rules (`owl:Nothing`, `disjointWith`, `AllDisjointClasses`, sameAs/differentFrom, functional literals), not full consistency checking |
| Ontology API | jena-ontapi `OntModel` object API | ✗ none (triples / SPARQL only) |
| SPARQL extensions | property functions (`list:member`, `apf:*`), `LET`, custom aggregates (`MEDIAN`, `MODE`, `FOLD`), `cdt:` list/map literals, JavaScript functions, full `afn:`/`fn:` library | ✗ none of the extensions; common `fn:`/`afn:`/`math:` functions only |
| SPARQL parser | JavaCC grammar | `spargebra`, which fails 7 W3C syntax/eval tests (see `tests/w3c-known-failures.txt`) |
| RDF formats | RDF Thrift, RDF Protobuf, TriX, RDF/JSON | ✗ (Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD only) |
| Change logs | RDF Patch (jena-rdfpatch), Fuseki `/patch` endpoint | ✗ none |
| Fuseki operations | Shiro authentication, per-graph access control (fuseki-access), Prometheus `/$/metrics`, assembler (`config.ttl`) service definitions, `/$/validate/*`, prefix read/write endpoints | Basic, Bearer tokens, OIDC (UI) and trusted proxy headers, with per-dataset levels; no graph-level ACLs yet; Prometheus `/$/metrics` with Sparkles metric names (not Fuseki's `fuseki_requests_*`), no JVM metrics; datasets are configured by CLI flags / admin API only; prefixes via `/{ds}/prefixes` |
| SERVICE | bulk / batched / cached SERVICE (serviceenhancer) | plain SERVICE only |
| Transactions over HTTP | — | — (same as Fuseki: one request = one transaction) |

### vs. QLever

| Area | QLever has | Sparkles |
|---|---|---|
| Scale | tested to tens of billions of triples (Wikidata, UniProt) | tested to 10.5M; the external-sort path is covered by tests but not measured at 100M+ |
| Streaming execution | lazy, block-wise evaluation of scans, joins, filters and GROUP BY; results streamed to the client | every operator materializes its full result (bounded by row and memory budgets); responses over 1 MiB are streamed to the client as they are serialized |
| Block prefiltering | FILTER ranges / STRSTARTS evaluated against block min/max to skip blocks | numeric range FILTERs on a scan's sort column read only the matching id ranges (inline integers and decimals); non-canonical numerals are still tested row by row |
| Pattern trick | `ql:has-predicate`, per-subject predicate patterns | ✗ (predicate counts use index runs instead) |
| Text / spatial | `ql:contains-word`, BM25 scoring, spatial joins, geo index | BM25 full-text search via `text:query` (no text/entity co-occurrence index); no spatial |
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
| Search | BM25 full-text, vector (HNSW), geospatial | BM25 full-text (`text:query`) and exact vector search (`spk:vectorSearch`); no approximate (HNSW) vector index or geospatial search yet |
| Deployment | S3 / DynamoDB / IPFS storage, Raft clustering, read replicas ("query peers") | single node, local disk |
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
| LZ4 instead of zstd for blocks; front coding instead of FSST for the vocabulary | Pure-Rust dependencies and very fast decoding. zstd/FSST remain a possible upgrade for compression ratio. |
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
| Out of scope for v1 | JS scripting functions, RDF Thrift/Protobuf/TriX, jena-ontapi object mapping, jena-text's Lucene index format and assembler configuration (Sparkles implements `text:query` itself), GeoSPARQL, ShEx, SHACL-AF rules (also absent in Jena), RDF Patch, backward-chaining (LP) rules, Shiro auth. |

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
| `--timeout S` | `60` | default query timeout in seconds (`timeout=` per request) |
| `--update-timeout S` | `0` | default SPARQL update timeout in seconds (`0`: none; `timeout=` per request); a timed-out update changes nothing |
| `--query-memory-mb N` | `8192` | budget for the estimated memory of a query's intermediate results (`0`: unlimited) |
| `--max-result-mb N` | `1024` | budget for the body of a query response (`0`: unlimited); Graph Store GET streams and has none |
| `--max-rows N` | `200000000` | rows of any intermediate result |
| `--vector-memory-mb N` | `4096` | memory for the packed vectors of `spk:vectorSearch`, per index generation |
| `--log-format text\|json` | `text` | log format on stderr (global flag); `RUST_LOG` filters as usual |
| `--no-access-log` | | no per-request log lines |
| `--no-metrics` | | `/$/metrics` answers `404` and no request metrics are kept |
| `--metrics-max-datasets N` | `100` | datasets with their own metric labels (the rest share `$other`) |
| `--otel` | off | export traces and metrics over OTLP (also enabled by `OTEL_EXPORTER_OTLP_ENDPOINT`; the standard `OTEL_*` variables apply, see `docs/API.md`, OpenTelemetry) |
| `--otel-logs` | off | export log events over OTLP too |
| `--otel-query-text` | off | record query text (`db.query.text`) and plan operator descriptions in spans; they may hold data |
| `--otel-plan-spans` | off | one span per executed plan operator |
| `--rate-limit SPEC` | off | limit a request class per client, e.g. `query=100/s,burst=200,concurrency=64` or `auth=10/min,burst=5` (repeatable; see `docs/API.md`, Rate limiting) |
| `--rate-limit-config FILE` | | JSON rate-limit configuration, re-read on SIGHUP; `--rate-limit` applies on top |
| `--rate-limit-trusted-proxy CIDR` | | proxy whose `Forwarded` / `X-Forwarded-For` names the client (repeatable) |

Over-budget requests fail with `507` and a JSON body naming the budget; the query stops as
soon as its client disconnects. `sparkles query --memory-mb N` applies the memory budget
on the command line (unlimited by default).

Command line tools (Jena `tdb2.*` / `arq` equivalents):

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load (tdb2.tdbloader)
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles query   --data file.ttl --query q.rq # query files in memory (arq --data)
sparkles update  --loc db 'INSERT DATA {...}'
sparkles compact --loc db                     # merge updates into a new generation
sparkles dump    --loc db > dump.nq
sparkles backup  --loc db --out backups/
sparkles clone   --loc db --to sandbox        # independent copy (same blank nodes, new dataset id)
sparkles stats   --loc db
sparkles log     --loc db                     # commit history (works next to a running server)
sparkles check   --loc db                     # verify the files, read-only (--quick, --format json)
sparkles infer   --loc db --profile owl-rl    # materialize inferences
sparkles infer   --loc db --status            # are the inferences up to date?
sparkles infer   --loc db --check             # OWL 2 RL inconsistency checks (exit 1 on violations)
```

`scripts/gen-data.py N` generates a synthetic dataset for benchmarking.

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

[`mise.toml`](mise.toml) pins Node, pnpm and hyperfine; Rust comes from `rust-toolchain.toml`.
It also defines the everyday tasks (`mise tasks` lists them all):

```sh
mise run build        # UI + release binary
mise run serve        # build, then serve ./data on :3030
mise run fmt          # cargo fmt + Prettier     (fmt:check for CI)
mise run lint         # clippy -D warnings + svelte-check
mise run test         # all Rust tests            (test:w3c, test:shacl for suite summaries)
mise run ui:test      # UI unit tests (Vitest)
mise run ci           # fmt:check + lint + test + ui:test
mise run gen-data 1000000 target/bench-data/10m.nt
mise run bench        # Sparkles vs Fuseki vs QLever; `bench 1000000 --runs 5` for 10.5M triples
mise run bench:shacl 100000; mise run bench:reasoner 100000 owl-rl
```

### Nix

The flake (flake-parts + rust-overlay, using the toolchain from `rust-toolchain.toml`)
provides:

* **Packages:**
  * `sparkles` (default): the binary with the UI embedded.
  * `sparkles-cli`: the same binary without the UI, so the build needs no Node.js.
  * `sparkles-ui`: the static UI build.
* **Other outputs:**
  * `overlays.default`;
  * a dev shell;
  * `checks`, including a NixOS VM test of the module behind nginx;
  * `nixosModules.default`.

```sh
nix run github:kclejeune/sparkles -- serve --data ./data
nix build .#sparkles-cli
nix flake check          # packages + NixOS VM test (Linux, needs KVM)
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
`openFirewall`). The nginx location sets:

* `client_max_body_size` to `nginx.clientMaxBodySize` (default 4g), for bulk uploads;
* proxy timeouts to `queryTimeout + 30` seconds;
* request/response buffering off, so large uploads and results stream through.

With `auth.configFile` the service starts with `--auth-config` and `systemctl reload
sparkles` re-reads it (SIGHUP). Keep the file out of the Nix store (agenix, sops-nix),
owned by the `sparkles` user. Do not also set nginx `basicAuthFile`: nginx would forward
its own `Authorization` header, which Sparkles would then reject. `unixSocket` makes the
server listen on a Unix socket that nginx proxies to, so trusted proxy headers can be
limited to it (`proxy.trusted = ["unix"]`).

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
prompt-injected model could otherwise send data to any URL. `--disable-tool NAME`
removes a tool. A database held by a running `sparkles serve` is refused (the lock);
only stdio is served for now. Logs go to stderr; stdout carries JSON-RPC only.

## Testing

```sh
cargo test --workspace
```

`crates/sparkles/tests/w3c.rs` runs the W3C SPARQL 1.0 / 1.1 query and update suites that
are vendored in the Apache Jena checkout (`../../apache/jena` next to this repository, or
`SPARKLES_W3C_DIR`). Known failures are listed in `crates/sparkles/tests/w3c-known-failures.txt`.
