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
| Compaction into a new generation (`gen-NNNN`, atomic `CURRENT` switch) | ✅ |
| Backups (gzipped N-Quads) | ✅ |
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
| Results: JSON, XML, CSV, TSV, `x-sparkles+json` (with executed plan); RDF: Turtle, N-Triples, N-Quads, TriG, JSON-LD, RDF/XML | ✅ |
| W3C conformance: SPARQL 1.1 query **328/328**, SPARQL 1.1 update **157/157**, SPARQL 1.0 **479/482**, SPARQL 1.2 **265/269** (all 7 failures are `spargebra` parser limitations, see `tests/w3c-known-failures.txt`) | ✅ |

### Server (Fuseki equivalent), reasoning, validation, UI

| Feature | Status |
|---|---|
| SPARQL protocol, GSP, upload, `/$/` admin (datasets, stats, compact, backup, tasks), Jena special graphs (`urn:x-arq:DefaultGraph`/`UnionGraph`) | ✅ |
| Jena-style CLI (`load`, `query`, `update`, `dump`, `compact`, `backup`, `stats`, `infer`, `shacl`), operating on the database directory directly | ✅ |
| Embedded Rust API (`sparkles::Dataset`) and fluent query builder (`sparkles::querybuilder`) | ✅ |
| RDFS / OWL 2 RL materialization, Jena rule syntax (`sparkles-reasoner`, `/$/reason`, `sparkles infer`) | ✅ |
| SHACL Core + SHACL-SPARQL validation (`sparkles-shacl`): W3C suite **98/98** Core, **20/20** SPARQL; parallel, index-backed | ✅ |
| Fuseki `/{ds}/shacl` endpoint (`graph=default\|union\|<iri>`, report as Turtle / N-Triples / JSON-LD / JSON, validates data ∪ inferences) and `sparkles shacl` command | ✅ |
| Query result cache controls: `--result-cache-mb`, `nocache=true`, cache stats in `/$/stats`, `POST /$/cache/clear/{ds}` | ✅ |
| SvelteKit UI: datasets, query editor, results table/graph/plan, explorer, schema browser (built against a mock; server integration pending) | 🚧 |

## Performance

See [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md). It compares Jena/Fuseki, QLever and
Fluree using hyperfine over HTTP, with every result cache off and each engine measured
on its own. Before timing, it checks that all engines return the same answers.

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.8 s** (Fluree 1.4, QLever 1.5, TDB2 4.3) | **6.1 s** (QLever 9.1, Fluree 10.2, TDB2 42.6) |
| Fastest of the four | 18 of 20 queries | 9 of 20 queries |
| Loses to QLever | `range-topk` 1.9× | `range-topk` 5.0×, `minus` 1.9×, `group-avg` 1.6×, `two-hop-count` 1.6×, `optional-count` 1.4×, `types-grouped` ≈ |
| Loses to Fluree | `two-hop-count` 1.5×, `distinct-obj` ≈ | `two-hop-count` 2.4×, `contains` 2.4×, `distinct-obj` 2.3×, `knows-reach` 1.75×, `lang-filter` 1.2×, `count-all` ≈ |
| vs. Fuseki | 1.8–34× faster; Fuseki errors on `foaf:knows*` | 1.8–400× faster |
| Update latency (1 triple, real insert) | **6.4 ms** (Fluree 6.5, QLever 11.8, Fuseki 41.7) | 10.9 ms (**Fluree 6.8**, QLever 15.8, Fuseki 38.6) |
| Throughput, 16 clients | **898 q/s** (Fluree 497, QLever 408, Fuseki 53) | **144 q/s** (QLever 57, Fluree 51, Fuseki 7) |
| Server memory | 229 MiB (**QLever 225**, Fuseki 1.7 GiB, Fluree 2.2 GiB) | 1.6 GiB (**QLever 362 MiB**, Fluree 3.1 GiB, Fuseki 3.9 GiB) |

Against QLever at 10.5M, Sparkles wins 14 of 20 queries, several by 8–17×
(`distinct-obj`, `regex-iri`, `contains`, `lang-filter`). Fluree is 1.6–61× slower
than Sparkles on general joins, OPTIONAL, subqueries, grouping and sorting, and was
OOM-killed (26 GB) on `optional-chain` at 10.5M.

Where Sparkles still loses on performance:
* **Range filters:** QLever skips blocks on range FILTERs (5× on `range-topk` at 10.5M).
* **Joins and grouping at scale:** at 10.5M, QLever is 1.4–1.9× faster on OPTIONAL,
  MINUS, GROUP BY with AVG and two-hop counts.
* **Count-only joins and single-predicate scans:** Fluree is 1.2–2.4× faster at 10.5M
  on `two-hop-count`, `contains`, `distinct-obj` and `lang-filter`, and 1.75× on the
  `foaf:knows*` traversal.
* **Update latency:** Fluree commits faster at 10.5M (it indexes in the background).
* **Memory:** Sparkles materializes every intermediate result and buffers whole
  responses. The table's 1.6 GiB at 10.5M was mostly heap retained by glibc. The
  server now uses mimalloc and releases free memory when idle, and ends the same run
  at 947 MiB, about 500 MiB of which is the block cache.
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
| Full-text search | jena-text (Lucene), `text:query` | ✗ none |
| Spatial | GeoSPARQL (`geof:` functions, spatial index) | ✗ none |
| Shape languages | ShEx (jena-shex) | ✗ SHACL only |
| Inference | on-the-fly `InfModel`, backward / hybrid rules (LP engine), OWL Micro/Mini/Full | forward materialization only (RDFS, OWL 2 RL subset, Jena forward rules); must be re-run after updates; no inconsistency detection (`owl:Nothing`, `disjointWith`) |
| Ontology API | jena-ontapi `OntModel` object API | ✗ none (triples / SPARQL only) |
| SPARQL extensions | property functions (`list:member`, `apf:*`), `LET`, custom aggregates (`MEDIAN`, `MODE`, `FOLD`), `cdt:` list/map literals, JavaScript functions, full `afn:`/`fn:` library | ✗ none of the extensions; common `fn:`/`afn:`/`math:` functions only |
| SPARQL parser | JavaCC grammar | `spargebra`, which fails 7 W3C syntax/eval tests (see `tests/w3c-known-failures.txt`) |
| RDF formats | RDF Thrift, RDF Protobuf, TriX, RDF/JSON | ✗ (Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD only) |
| Change logs | RDF Patch (jena-rdfpatch), Fuseki `/patch` endpoint | ✗ none |
| Fuseki operations | Shiro authentication, per-graph access control (fuseki-access), Prometheus `/$/metrics`, assembler (`config.ttl`) service definitions, `/$/validate/*`, prefix read/write endpoints | ✗ no auth or ACLs (run behind a proxy); no metrics; datasets are configured by CLI flags / admin API only; prefixes are read-only |
| SERVICE | bulk / batched / cached SERVICE (serviceenhancer) | plain SERVICE only |
| Transactions over HTTP | — | — (same as Fuseki: one request = one transaction) |

### vs. QLever

| Area | QLever has | Sparkles |
|---|---|---|
| Scale | tested to tens of billions of triples (Wikidata, UniProt) | tested to 10.5M; the external-sort path is covered by tests but not measured at 100M+ |
| Streaming execution | lazy, block-wise evaluation of scans, joins, filters and GROUP BY; results streamed to the client | every operator materializes its full result (bounded by a row limit); HTTP responses are serialized to a buffer before sending |
| Block prefiltering | FILTER ranges / STRSTARTS evaluated against block min/max to skip blocks | not implemented (this is why `range-topk` loses) |
| Pattern trick | `ql:has-predicate`, per-subject predicate patterns | ✗ (predicate counts use index runs instead) |
| Text / spatial | `ql:contains-word`, BM25 scoring, spatial joins, geo index | ✗ |
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
| History | immutable commit chain (content-addressed), time travel (`@t:`, `@iso:`, `@commit:`), history queries, branches / merge / revert | ✗ MVCC snapshots only; no history after compaction |
| Security | ledger-stored access policies, JWS / `did:key` signed requests and commits, OIDC, encryption at rest | ✗ none (run behind a proxy) |
| Interfaces | JSON-LD transactions and queries (FQL), openCypher + Bolt, GraphQL, SQL / R2RML / Iceberg graph sources, MCP server | SPARQL and the Rust API only; JSON-LD as an RDF format only |
| Search | BM25 full-text, vector (HNSW), geospatial | ✗ |
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
| Reasoning is materialized (forward chaining into the `urn:x-sparkles:inferred` graph, queried as default ∪ inferred) instead of Jena's on-the-fly `InfGraph` | Query speed stays that of the plain index. The trade-off is re-running `/$/reason` after updates. Backward (LP) rules are not supported. |
| `AS ?v` targets that are already in scope are rejected (SPARQL §18.2.1) | `spargebra` does not check this, so Sparkles validates it itself, matching Jena and QLever. |
| Out of scope for v1 | JS scripting functions, RDF Thrift/Protobuf/TriX, jena-ontapi object mapping, jena-text, GeoSPARQL, ShEx, SHACL-AF rules (also absent in Jena), RDF Patch, backward-chaining (LP) rules, Shiro auth. |

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

Command line tools (Jena `tdb2.*` / `arq` equivalents):

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load (tdb2.tdbloader)
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles query   --data file.ttl --query q.rq # query files in memory (arq --data)
sparkles update  --loc db 'INSERT DATA {...}'
sparkles compact --loc db                     # merge updates into a new generation
sparkles dump    --loc db > dump.nq
sparkles backup  --loc db --out backups/
sparkles stats   --loc db
sparkles infer   --loc db --profile owl-rl    # materialize inferences
```

`scripts/gen-data.py N` generates a synthetic dataset for benchmarking.

### mise tasks

[`mise.toml`](mise.toml) pins Node, pnpm and hyperfine; Rust comes from `rust-toolchain.toml`.
It also defines the everyday tasks (`mise tasks` lists them all):

```sh
mise run build        # UI + release binary
mise run serve        # build, then serve ./data on :3030
mise run fmt          # cargo fmt + Prettier     (fmt:check for CI)
mise run lint         # clippy -D warnings + svelte-check
mise run test         # all Rust tests            (test:w3c, test:shacl for suite summaries)
mise run ci           # fmt:check + lint + test
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
            # readOnly = true; allowService = false; unionDefaultGraph = true;
            nginx = {
              enable = true;
              virtualHost = "sparql.example.org";
            };
          };
          # standard nginx semantics: TLS, auth, extra locations …
          services.nginx.virtualHosts."sparql.example.org" = {
            enableACME = true;
            forceSSL = true;
            basicAuthFile = "/run/secrets/sparkles-htpasswd"; # the server has no auth of its own
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

The CLI goes on the system path unless `installCli = false`. The server holds a lock on
its databases, so for offline work (`sparkles load`, `compact`) stop the service first,
or use the HTTP API.

## Testing

```sh
cargo test --workspace
```

`crates/sparkles/tests/w3c.rs` runs the W3C SPARQL 1.0 / 1.1 query and update suites that
are vendored in the Apache Jena checkout (`../../apache/jena` next to this repository, or
`SPARKLES_W3C_DIR`). Known failures are listed in `crates/sparkles/tests/w3c-known-failures.txt`.
