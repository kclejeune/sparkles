# Provenance of adopted designs and dependencies

This is the clean-room provenance record of Sparkles. Each entry covers one feature or
dependency. It says where the design came from, what was adopted, with version and
license, and what was rejected. Each feature's spec in this directory (see
the [index](README.md)) gives the full design, and its Outcome section describes how the
implementation landed.

## Allocator (server and CLI binaries)

- **Adopted:** `mimalloc` 0.1.52 (MIT), with `libmimalloc-sys` 0.1.49 (MIT). The
  `extended` feature of `libmimalloc-sys` provides `mi_collect`. The crate bundles
  Microsoft mimalloc 3.3.2 (MIT). The allocator is a default-on feature of
  `sparkles-server`. The `sparkles` library stays allocator-neutral.
- **Design source:** our own measurements of allocator memory retention; the mimalloc
  option documentation and source (`src/options.c`: `purge_delay`, `purge_decommits`);
  and glibc `mallopt(3)` and `malloc_trim(3)`.
- **Rejected:**
  - glibc `mallopt` fixed thresholds: slower throughput.
  - `tikv-jemallocator` 0.7 (MIT/Apache-2.0): slower, and needs `make`.
  - mimalloc `PURGE_DELAY=0`: slower throughput.

## Durable commit identity

- **Spec:** [`CI-commit-identity.md`](CI-commit-identity.md), written independently from
  the Sparkles code, the Apache Jena Fuseki sources (Apache-2.0), the W3C SPARQL 1.1
  Protocol and Graph Store Protocol, and RFCs 3339, 9562, 9110, 6648 and 9651.
- **Implementation:** from the spec plus Sparkles code only.
  - **Dependencies:** `uuid` 1.x (Apache-2.0/MIT), with its `serde` feature now
    enabled, and the CRC-32 from `flate2` (MIT/Apache-2.0). Both were already
    dependencies.
- **Rejected** (spec §8):
  - `Snapshot::version` as the id;
  - content hashes as the primary id;
  - a new WAL opcode, which older binaries would truncate;
  - fsyncing the catalog per commit;
  - a JSON-lines catalog;
  - empty commits for no-op writes;
  - a commit per compaction.

## Full-text search

- **Spec:** [`F03-full-text-search.md`](F03-full-text-search.md), written independently
  from:
  - the Sparkles code;
  - Apache Jena's text-query documentation and `jena-text` sources (Apache-2.0), for
    syntax compatibility only;
  - the Tantivy docs and spargebra's parser source;
  - the SPARQL 1.1 Query §4.2.3 collections syntax, BCP 47 and RFC 4647.
- **Adopted:** `tantivy` 0.26.2 (MIT) with default features off, plus `mmap`, `stemmer`,
  `stopwords` and `lz4-compression`. That set avoids zstd's C code. The `stemmer` feature
  links `rust-stemmers` 1.2.0 (MIT OR BSD-3-Clause), the Snowball stemmers that the
  per-language analyzers use. The `stopwords` feature adds no crate. Its lists are part of
  Tantivy's source. The English list is Lucene's, and the others are the Snowball
  project's (BSD-3-Clause, the license `rust-stemmers` already carries). Tantivy is behind the optional
  `text` feature of `sparkles`, which the server enables by default.
- **Rejected** (spec §8):
  - a home-grown inverted index;
  - one document per subject;
  - keys from vocabulary ids;
  - an index inside the generation directories;
  - asynchronous indexing;
  - an in-memory overlay for recent updates;
  - post-filtering graphs after top-k;
  - a special text datatype;
  - a FILTER function as the main interface;
  - external engines (Elasticsearch, SQLite FTS5).

## Vector similarity and vector indexes

- **Spec:** [`F04-vector-search.md`](F04-vector-search.md), written independently from
  RDF 1.2 Concepts, SPARQL 1.1 §17.6, RFC 8259, IEEE 754, RFC 8141, the HNSW paper,
  docs.rs / crates.io pages for the ANN candidates, and the Sparkles code.
- **Implementation:** Phase 1 (exact search), from the spec and Sparkles code only.
  There are no new dependencies. The kernel is our own 8-lane loop, and rayon was
  already in use.
- **Configured indexes and HNSW (Phase 1b and 2):** from the spec, the Sparkles code
  (the spatial index's background builds and mapped files were the model), and the
  HNSW paper (Malkov and Yashunin, arXiv:1603.09320: Algorithms 1, 2, 4 and 5, the level
  distribution of §4, and the parameter names M, efConstruction and ef). The graph is our
  own code. No new dependency: it uses parking_lot, rayon, memmap2 and flate2's CRC-32,
  which Sparkles already links.
- **Library choice.** USearch 2.26.2 (Apache-2.0), hnsw_rs 0.3.4 (MIT OR Apache-2.0) and
  instant-distance 0.6.1 (MIT OR Apache-2.0) were measured in a throwaway harness through
  their public APIs only, on 100k clustered vectors of dimension 384 and 768 (cosine,
  M = 16, efConstruction = 128, 1000 queries), next to the graph written for Sparkles.
  None of their source was copied. At 384 dimensions and ef = 64, recall@10 and median
  latency were 0.997 and 0.35 ms for USearch (f32 or f16), 0.981 and 0.78 ms for
  hnsw_rs, 0.999 and 0.37 ms for the Sparkles graph, and instant-distance needed 137 s to
  build. USearch and hnsw_rs keep their own copy of every vector (USearch f16 added
  145 MB, hnsw_rs 375 MB), while the Sparkles graph reads the packed vectors and added
  13 MB. USearch's Rust binding sets `ef` per index rather than per query, needs a C++
  toolchain and reserved thread slots, and is not unwind-safe. hnsw_rs adds about a
  dozen crates (env_logger, bincode 1, mmap-rs, rand 0.9 among them). instant-distance
  fixes `ef` at build time and has no filtered search. At 1M × 384, the Sparkles graph
  reached recall@10 of 0.929 at ef = 64 (0.29 ms) and USearch f16 0.908 (0.36 ms).
  USearch built 2.5 times faster and held 1.3 GB against 130 MB. No ANN crate was added.
- **Rejected** (spec §8):
  - canonicalizing vector literals on load;
  - a new id tag for vectors;
  - `rdf:JSON`;
  - maintaining the graph on the commit path.

## Embeddings computed on write

- **Spec:** [`F08-embeddings-on-write.md`](F08-embeddings-on-write.md), written on
  2026-10-02 independently from:
  - the Sparkles code and the [F04](F04-vector-search.md), [CI](CI-commit-identity.md),
    [C10](C10-write-time-validation.md) and [C13](C13-automatic-compaction.md) specs;
  - OpenAI's API reference for `POST /v1/embeddings`, and the OpenAI-compatible
    embeddings endpoints documented by Ollama, vLLM, LM Studio and Hugging Face Text
    Embeddings Inference;
  - the public documentation of Timescale pgai (the vectorizer and its worker), Weaviate
    (the `text2vec-openai` and `text2vec-ollama` modules, `nearText`), Elasticsearch (the
    inference API, `semantic_text`), Neo4j (the GenAI procedures) and Qdrant (FastEmbed),
    read for behaviour only;
  - RFC 9110, RFC 6585, RFC 6750 and RFC 4647, and the crates.io pages of `fastembed`
    and `candle-core`.

  The roadmap item that named the feature came from a review of other databases'
  public descriptions. The spec itself was written from the sources above.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. The client, the
  worker, the input record and the mock endpoint are our own code. **Dependencies:**
  none new. Requests use the `reqwest` client and the outbound policy the engine already
  has, and the cache uses `quick_cache`, already a dependency.
- **Local models.** No model runtime was added. `fastembed` 7.1.0 (Apache-2.0) runs
  models through ONNX Runtime, a native library its default features download at build
  time, and fetches models from Hugging Face. Candle (MIT OR Apache-2.0) is pure Rust
  but would bring model files, a tokenizer and CPU-heavy inference into the server.
  Local models run behind Ollama or another OpenAI-compatible server instead. The binary
  size of either option was not measured.
- **Rejected** (spec §8):
  - embedding on the commit path;
  - vectors outside the RDF data;
  - replaying the change feed to find changed text;
  - API keys in `vector.json` or in API bodies;
  - a model runtime in the binary;
  - always one vector per subject.

## Path search

- **Spec:** [`F07-path-search.md`](F07-path-search.md), written on 2026-10-02
  independently from:
  - the Sparkles code and the [C01](C01-observability-and-budgets.md),
    [C12](C12-graph-access-control.md), [C12b](C12b-triple-access-control.md),
    [F01](F01-cypher.md) and [F04](F04-vector-search.md) specs;
  - SPARQL 1.1 Query §9 and the SPARQL working group's feature pages on path lengths and
    path binding, and the RDF 1.2 Concepts and Turtle 1.2 drafts for reifiers;
  - QLever's `PathSearch` and `PathQuery` sources (Apache-2.0), read for the parameters
    and the result shape;
  - the public documentation of Ontotext GraphDB (graph path search), Stardog (path
    queries), Neo4j (shortest paths) and OpenLink Virtuoso (transitivity in SPARQL and
    SQL), and public summaries of the GQL path selectors and path modes, read for
    behaviour only;
  - Pohl (1971) on bidirectional search, Dijkstra (1959) and Yen (1971).

  The review of other databases that named the roadmap item was not used.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. The searches,
  the adjacency scans and the planner's handling of the service are our own code, and
  the index sweep follows the transitive path operator Sparkles already had.
  **Dependencies:** none new.
- **Rejected** (spec §8):
  - a `PATHS` query form or `TRANSITIVE` options, which need new grammar;
  - a property function with list arguments;
  - one value per path, such as a list literal or JSON;
  - walk and trail semantics;
  - weights on nodes or predicates instead of on reifiers;
  - truncating a search that visits too many nodes;
  - reusing the transitive path operator.

## Named snapshots and point-in-time reads

- **Spec:** [`F06-snapshots-and-point-in-time.md`](F06-snapshots-and-point-in-time.md),
  written on 2026-09-30 independently from:
  - the Sparkles code and [CI](CI-commit-identity.md);
  - RFC 7089 (Memento), RFC 9110, and the W3C SPARQL 1.1 Protocol and Graph Store
    Protocol, all fetched;
  - RFCs 3339, 8246, 6648 and 9651, cited from general knowledge;
  - Jena's RDF Patch format (Apache-2.0), cited from general knowledge for an open
    question only.
- **Implementation:** from the spec plus Sparkles code only (2026-09-30).
  - **Dependencies:** `httpdate` 1.0.3 (MIT OR Apache-2.0), new in the server.
- **Rejected** (spec §8):
  - alternatives B (a new write path and formats) and C (named points only, a rebuild per
    pin) as the MVP;
  - in-memory `Arc<Snapshot>` retention for persistent datasets;
  - generation names in pins;
  - deleting a pin's data when its generation is missing;
  - `404` for garbage-collected commits (`410` instead);
  - RFC 7089 clamping for `?at=time:` before the root;
  - a strong ETag per commit;
  - refusing compaction when retention would exceed a limit;
  - a `sealed.json` per generation;
  - `Snapshot::version` in the cache key of historical snapshots;
  - `X-Sparkles-*` or product-specific selector syntaxes.

## Authentication and dataset-level access control

- **Spec:** [`C09-dataset-access-control.md`](C09-dataset-access-control.md), written on
  2026-09-30 independently from:
  - the Sparkles code;
  - the maintainer's own nimbus project (CLI browser, loopback and device login flows),
    read with the maintainer's permission;
  - Apache Jena Fuseki's "Data Access Control" documentation (Apache-2.0);
  - RFCs 9110, 7617, 6750, 6749, 7636 (PKCE), 8628 (device grant), 8252, 8414, 7519,
    7517, 6265 and 3339; OpenID Connect Core, Discovery and RP-Initiated Logout 1.0;
    W3C Solid WAC and the SPARQL 1.1 Protocol;
  - the OWASP Password Storage, CSRF, Session Management and HTML5 Security cheat sheets;
  - docs.rs pages for `argon2`, `openidconnect`, `axum-extra`, `axum` and `tower-http`,
    and the public docs of oauth2-proxy, Authelia, Tailscale serve and Cloudflare Access
    (header names only).
- **Implementation:** from the spec plus Sparkles code only (2026-09-30), behind the
  default-on `auth` feature of `sparkles-server`.
  - **Dependencies:**
    - `argon2` 0.6.0 (MIT OR Apache-2.0), with `password-hash` 0.6.1 and `blake2` 0.11.0
      (MIT OR Apache-2.0), for password hashes;
    - `jsonwebtoken` 11.1.0 (MIT), default features off, with the `aws_lc_rs` backend,
      for OIDC ID-token signatures only. `aws-lc-rs` 1.18.1 (ISC AND (Apache-2.0 OR
      ISC)) and `aws-lc-sys` 0.45.0 (ISC, Apache-2.0, MIT and BSD-3-Clause parts) were
      already in the tree through rustls;
    - `subtle` 2.6.1 (BSD-3-Clause), `zeroize` 1.9.0 (Apache-2.0 OR MIT), `getrandom`
      0.3.4 (MIT OR Apache-2.0), `toml` 1.1.6 (MIT OR Apache-2.0), `rpassword` 7.5.4
      (Apache-2.0), and `ipnet` 2.12.2 (MIT OR Apache-2.0, not optional);
    - `sha2` and `reqwest` (already dependencies), and clap's `env` feature;
    - tests only: `aws-lc-rs` 1 as a dev-dependency, to sign the mock OIDC provider's
      tokens.
  - **Not adopted:** `openidconnect` 4.0.1 (MIT) and `cookie`, which the spec named.
    Our own code over `jsonwebtoken`, `reqwest` and `sha2` implements the OIDC relying
    party (discovery, code flow with PKCE, claim checks), HMAC-SHA-256 and cookie
    handling.
- **Rejected** (spec §14):
  - authentication only in the reverse proxy;
  - a JS auth server (better-auth) or SvelteKit SSR for OIDC;
  - tokens or passwords in browser storage;
  - stateless signed session cookies, and a memory-only session store;
  - argon2id for API tokens;
  - self-contained JWT Sparkles tokens;
  - the token in the loopback redirect URL;
  - scope checks at mint time only;
  - Shiro-style URL rules, or Solid WAC ACL documents;
  - deny rules, and `403` for hidden datasets;
  - trusting proxy headers from any peer, or behind a global flag.

## Graph-level access control and endpoint permissions

- **Spec:** [`C12-graph-access-control.md`](C12-graph-access-control.md), written on
  2026-10-02 independently from:
  - the Sparkles code and the specs C09, C10, C11, F03, F04 and F06;
  - Apache Jena Fuseki's "Data Access Control for Fuseki" documentation (Apache-2.0),
    for `access:AccessControlledDataset`, `access:entry`, `urn:x-arq:DefaultGraph` in
    entries, and `allowedUsers` at the server, dataset and endpoint levels;
  - W3C Solid Web Access Control and Access Control Policy, the NIST RBAC model and
    SPARQL 1.1 Query, Update, Protocol and Graph Store Protocol, cited from working
    knowledge.
- **Implementation:** from the spec plus Sparkles code only (2026-10-02). The engine part
  is in `sparkles::access` and the query, update, store, schema and diff modules. The
  server part is in the `auth` module of `sparkles-server` and its handlers.
  - **Dependencies:** none added.
- **Rejected** (spec §11):
  - filtering in the HTTP handlers instead of the engine;
  - turning off the fast paths or the result cache for restricted principals;
  - checking writes against the changes that took effect;
  - making `CLEAR ALL` fail for every restricted principal;
  - deny rules, Solid WAC ACL documents, and a parser for Fuseki's `access:` assembler
    vocabulary.

## Protections of triples

- **Spec:** [`C12b-triple-access-control.md`](C12b-triple-access-control.md), written on
  2026-10-02 independently from:
  - the Sparkles code and the specs C09, C12, C15, C16 and F06;
  - the source and readme of Apache Jena's `jena-permissions` module (Apache-2.0) at
    the `jena-5.6.0` release, the last before it was retired: `SecurityEvaluator`,
    `SecuredGraph` and the query rewriter `OpRewriter`;
  - AllegroGraph's security filters documentation (fetched 2026-10-02) and MarkLogic's
    element-level security documentation (protected paths and protected path sets,
    found 2026-10-02);
  - W3C Solid Web Access Control and Access Control Policy, the W3C ODRL Information
    Model 2.2, OASIS XACML 3.0, SPARQL 1.1 and RDF Schema, cited from working knowledge.
- **Implementation:** from the spec plus Sparkles code only (2026-10-02). The engine part
  is in `sparkles::access::triples`, the snapshot's mask, the write transaction's checks
  and the query, update, schema, diff and change-feed modules. The server part is in the
  `auth` module of `sparkles-server` and the Graph Store and listing handlers.
  - **Dependencies:** none added.
- **Rejected** (spec §12):
  - checking each triple during evaluation, as `jena-permissions` does;
  - rewriting queries with filters;
  - turning off the fast paths for protected callers;
  - allow and disallow filters attached to each grant;
  - lists of readers in the protection;
  - inference per caller;
  - exact per-commit masks for the change feed;
  - hiding IRIs as well as triples.

## Write previews

- **Spec:** [`C15-write-previews.md`](C15-write-previews.md), written on 2026-10-02
  independently from:
  - the Sparkles code and the specs CI, C10, C12 and G02;
  - the W3C SPARQL 1.1 Graph Store HTTP Protocol (PUT, POST and PATCH semantics);
  - the Kubernetes documentation of server-side dry run (the `dryRun` parameter, the
    same pipeline and authorization, admission without side effects);
  - the RDF4J `RepositoryConnection` Javadoc (`prepare` and `rollback`);
  - SPARQL 1.1 Update and Protocol, RFC 9110, RFC 4918, RFC 6648, RFC 7240 and Apache
    Jena's RDF Patch, cited from working knowledge.
- **Implementation:** from the spec plus Sparkles code only.
  - **Dependencies:** none added.
- **Rejected** (spec §8):
  - a separate preview endpoint and `Prefer: dry-run`;
  - holding a previewed transaction for a later commit;
  - a `200` for every outcome, and stopping at the first refusal;
  - turning bulk writes into transactional ones for a preview;
  - upsert and graph-sync verbs, which SPARQL Update and Graph Store `PUT` already cover.

## Shapes drafted from the data

- **Spec:** Phase 4 of [`C02-schema-discovery.md`](C02-schema-discovery.md#11-phase-4-shapes-drafted-from-the-data),
  written on 2026-10-02 independently from:
  - W3C SHACL (targets of `sh:targetClass` as SHACL instances, the value-type,
    cardinality, value-range, string-based and closed-shape components) and ShEx 2.1
    (triple constraints, node constraints, value sets, `EXTRA`, `CLOSED`, query shape
    maps), cited from working knowledge;
  - published shape-extraction work, cited from working knowledge: QSE (Rabbani,
    Lissandrini and Hose, PVLDB 2023) for the entity-to-types map and support pruning,
    sheXer (Fernández-Álvarez, Labra Gayo and Gayo-Avello, Knowledge-Based Systems 2022)
    for a trust threshold per constraint, and ABSTAT (Spahiu et al., 2016) for type
    patterns with cardinality statistics. No code of these projects was read or used;
  - the Sparkles code and specs C02, C10, C11 and C12.
- **Implementation:** from the spec plus Sparkles code only (2026-10-02), in
  `sparkles::schema::draft` and the server's schema, MCP and CLI code.
  - **Dependencies:** none added.
- **Rejected** (C02 §11.7):
  - SPARQL `GROUP BY` queries per class instead of index passes;
  - sampling;
  - support counted over every instance for value constraints;
  - `sh:or` of datatypes and `sh:node` references between class shapes;
  - writing drafts into a shapes graph of the dataset.

## Stored, parameterized queries

- **Spec:** [`C16-stored-queries.md`](C16-stored-queries.md), written on 2026-10-02
  independently from:
  - W3C SPARQL 1.1 Query and Protocol, cited from working knowledge;
  - Apache Jena's documentation (Apache-2.0) of `ParameterizedSparqlString` and of query
    execution with substitutions, cited from working knowledge;
  - the public documentation of Stardog's stored queries and of Ontotext GraphDB's saved
    queries, cited from working knowledge;
  - the OpenAPI Specification's typed operation parameters and the MCP specification's
    tool input schemas, cited from working knowledge;
  - the Sparkles code and specs CI, C01, C09, C11 and C12.
- **Implementation:** from the spec plus Sparkles code only (2026-10-02). The catalog,
  the parameter types and the binding are in `sparkles::stored`. The admin API, runs, MCP
  tools and the `sparkles queries` command are in `sparkles-server`.
  - **Dependencies:** none added.
- **Rejected** (spec §8):
  - text substitution with escaping;
  - a trailing `VALUES` clause;
  - `$name` parameters only;
  - a reserved graph for the definitions;
  - one generic MCP tool for every stored query;
  - per-user saved queries.

## GraphQL read adapter

- **Spec:** [`C03-graphql.md`](C03-graphql.md), written on 2026-10-02 independently
  from:
  - the GraphQL specification, October 2021 edition and the current working draft, the
    GraphQL over HTTP working draft and the GraphQL Cursor Connections specification,
    fetched 2026-10-02;
  - GraphQL-LD (Taelman, Vander Sande and Verborgh, ISWC 2018 demo), cited from working
    knowledge, and the README of `graphql-to-sparql` (MIT). No code was used;
  - HyperGraphQL's README (Apache-2.0), the public documentation of Stardog's GraphQL
    support and of Ontotext's Semantic Objects (SOML queries and properties), Hasura's
    filter documentation and GitHub's GraphQL API limits, fetched 2026-10-02;
  - PostGraphile's and Apollo Server's conventions, cited from working knowledge;
  - the W3C Bridging GraphQL and RDF Community Group's charter page;
  - Hartig and Pérez, "Semantics and Complexity of GraphQL" (WWW 2018), and Cha et al.,
    "A Principled Approach to GraphQL Query Cost Analysis" (ESEC/FSE 2020), cited from
    working knowledge;
  - the crates.io and docs.rs pages of `apollo-compiler`, `apollo-parser`,
    `async-graphql` and `juniper`, and the npm records of `cm6-graphql`, `graphql` and
    `graphiql`;
  - the Sparkles code and specs C01, C02, C09, C10, C11, C12, C16 and F06.
- **Implementation:** Phase 1, from the spec plus Sparkles code only (2026-10-02). The
  mapping schema, the API schema, the planner, the algebra of the fetch groups, the
  executor and the drafts are in the new crate `sparkles-graphql`. The routes, the
  `graphql` endpoint of grants and `sparkles graphql` are in `sparkles-server`, behind its
  default-on `graphql` feature. The engine gained `QueryOptions::work`, a row count shared
  by several queries.
  - **Dependencies:** `apollo-compiler` 1.33.0 and `apollo-parser` 0.8.6 (MIT OR
    Apache-2.0). The crate also uses `quick_cache` 0.7 and `serde_json_bytes` 0.2, which
    were already in the tree. `Cargo.lock` gained `ariadne` 0.6, `typed-arena` 2,
    `jsonpath-rust` 0.3 (MIT, through a license file) and `is-terminal` 0.4 (MIT), and
    `rowan` 0.16, `serde_json_bytes` 0.2, `triomphe` 0.1, `countme` 3, `text-size` 1,
    `ahash` 0.8, `yansi` 1, `concolor` 0.1, `concolor-query` 0.3, `pest` 2.9 with its
    derive, generator and meta crates, `ucd-trie` 0.1, `hashbrown` 0.14, `rustc-hash` 1.1,
    `hermit-abi` 0.5 and the `windows-sys` 0.45 family (MIT OR Apache-2.0). The license
    of every crate was read with `scripts/third-party-licenses.py`, and
    `THIRD_PARTY_LICENSES.md` was regenerated. No UI package was added, since the UI
    page is Phase 2.
- **Rejected** (spec §17):
  - one SPARQL query per document with nested `OPTIONAL`s, as GraphQL-LD builds;
  - a resolver per field with `juniper` (BSD-2-Clause) or `async-graphql`'s dynamic
    schemas (MIT OR Apache-2.0);
  - a schema that follows the data, as Stardog's automatic schema does;
  - generating SPARQL text instead of algebra;
  - keyset cursors for every order, and cursors at the head;
  - compacted identifiers;
  - a schema per principal;
  - embedding GraphiQL (MIT, React);
  - Hasura's `_eq` operator names;
  - Apollo's automatic persisted queries;
  - a SPARQL passthrough field;
  - mutations, for the reasons of spec §12.

## Write-time SHACL validation

- **Spec:** [`C10-write-time-validation.md`](C10-write-time-validation.md), written on
  2026-09-30 independently from:
  - the Sparkles code and specs (CI, C08, C06, C01);
  - W3C SHACL, SHACL 1.2 Core (Working Draft), SPARQL 1.1 Update and Protocol, and
    Apache Jena's SHACL documentation (Apache-2.0), all fetched;
  - RFC 9110 and RFC 9651, cited from general knowledge;
  - four papers on integrity checking and view maintenance, cited from general
    knowledge: Nicolas 1982; Blakeley, Larson and Tompa 1986; Gupta and Mumick 1995;
    Corman, Reutter and Savković 2018.

  TopBraid's documentation was not consulted.
- **Implementation:** from the spec plus Sparkles code only (2026-09-30). There are no
  new third-party dependencies. `sparkles-shacl` now uses `serde`, `serde_json`,
  `parking_lot`, `sha2` and `tempfile`, which were all already in the workspace.
- **Rejected** (spec §9):
  - validating after commit and reverting with a compensating commit;
  - validating only the request's data;
  - validating each operation of a multi-operation update;
  - a guard in the HTTP layer only;
  - validating outside the writer lock;
  - validating the bulk batch through `view()`;
  - `409 Conflict` or `400 Bad Request` for rejected writes (`422` instead);
  - referencing the shapes file by path;
  - rejecting on any result (`sh:conforms false`);
  - a default reserved shapes graph name;
  - honoring `sh:shapesGraph` triples in the data;
  - diff semantics (new results only) as the Phase 1 default.

## MCP server

- **Spec:** [`C11-mcp-server.md`](C11-mcp-server.md), written on 2026-09-30
  independently from:
  - the MCP specification, revision `2026-07-28`, fetched from modelcontextprotocol.io.
    The parts read were versioning, the changelog, the base protocol, the stdio and
    streamable HTTP transports, tools, resources, prompts, pagination, caching,
    cancellation, authorization and the schema;
  - the rmcp 3.5.0 crates.io record, its docs.rs pages, READMEs and `LICENSE`;
  - JSON-RPC 2.0 and SPARQL 1.1, cited from working knowledge;
  - the Sparkles code and specs C01, C02 and C06.
- **Implementation:** from the spec plus Sparkles code only (2026-09-30), behind the
  default-on `mcp` feature of `sparkles-server`.
  - **Dependencies:** `rmcp` 3.5.0 (Apache-2.0), the official Rust SDK
    (github.com/modelcontextprotocol/rust-sdk). It is pinned to `~3.5` with default
    features off and only `server` and `transport-io` on, so there are no tool macros.
    Its dependencies are MIT, Apache-2.0 or both, including `schemars` 1.2.2 (MIT). The
    published crate carries no `LICENSE` file. The license text is in the upstream
    repository.
  - **HTTP transport** (2026-10-02): rmcp's `transport-streamable-http-server` feature
    is on as well, for `/$/mcp`. It adds one crate, `sse-stream` 0.2.6 (MIT OR
    Apache-2.0), which turns HTTP bodies into SSE streams. `tokio-util` 0.7 (MIT) became
    a direct dependency of the `mcp` feature, for the token that ends the endpoint's
    streams at shutdown. It was already in the tree through rmcp. The transport, its
    session rules and the auth integration follow the C11 spec and the C09 permission
    model. No other MCP server was consulted.
- **Rejected** (spec §9):
  - a hand-rolled JSON-RPC layer (the fallback, isolated behind `adapter.rs`);
  - rmcp `#[tool]` macros with `schemars`-derived schemas;
  - one generic `sparql` tool only;
  - SPARQL JSON results as tool output;
  - `structuredContent` plus the same rows as JSON text;
  - protocol sessions or opaque snapshot handles;
  - silently injecting `LIMIT`;
  - listing the search tools only when the data supports them;
  - SSE responses and progress notifications.

## Compression codecs

- **Spec:** [`X01-compression-codecs.md`](X01-compression-codecs.md), written on
  2026-09-30 independently as internal engineering. Its sources are:
  - the codec formats' own specifications: RFC 8878 (zstd), RFC 7932 (Brotli), RFC 1952
    (gzip) and the LZ4 frame format;
  - RFC 9110 content codings;
  - the docs of `zstd`, `ruzstd`, `brotli`, `flate2`, `lz4_flex`, tower-http 0.7,
    rust-embed 8 and Tantivy 0.26;
  - the Sparkles code.

  It does not derive from any other database product.
- **Implementation:** Phase 1, from the spec plus Sparkles code only (2026-09-30).
  - **Dependencies:**
    - `zstd` 0.14.0 (BSD-3-Clause), with `zstd-safe` 8.0.0, `zstd-sys`
      2.1.0+zstd.1.5.7 (BSD-3-Clause) and the `zstdmt` feature. `zstd-sys` is a C build
      of libzstd 1.5.7. The crate is behind the `zstd` feature of `sparkles`, which is
      off in the library and on in the server. The spec's table says MIT/Apache-2.0, but
      the crate's `Cargo.toml` says BSD-3-Clause. Version 0.13 was MIT.
    - `brotli` 8.0.4 (BSD-3-Clause AND MIT), pure Rust, with `brotli-decompressor` 5.0.3
      (BSD-3-Clause/MIT), `alloc-no-stdlib` 2.0.4 and `alloc-stdlib` 0.2.4
      (BSD-3-Clause). It is behind the `brotli` feature, which is also off in the
      library and on in the server.
    - tower-http 0.7.1 (MIT) `decompression-full`, which adds `async-compression` 0.4.49
      and `compression-codecs` 0.4.44 (MIT OR Apache-2.0) over the same codec crates.
    - Tantivy's `zstd-compression` feature, which uses `zstd` 0.13.3 (MIT, `zstd-safe`
      7.3.0) over the same `zstd-sys`.
    - `http-body-util` 0.1.5 (MIT), new in the server. `flate2` 1.1.10 and `lz4_flex`
      0.14.0 were already dependencies.
    - The UI's precompression step (`ui/scripts/precompress.mjs`) uses Node's built-in
      zlib only.
  - **Not adopted:** `ruzstd` (MIT) and the `zstd-decode-pure` feature (spec §3).
- **Deferred:** Phase 2 storage codecs for index blocks, vocabulary and spill files,
  which wait on measurements. Phase 3 repository blobs, together with F05.
- **Rejected** (spec §1 non-goals, since the spec has no rejected-alternatives section):
  - compressing the WAL, `delta.vocab` or `commits.bin`;
  - in-memory compressed caches;
  - compressed network protocols other than HTTP `Content-Encoding`.

## Backup repositories

- **Spec:** [`F05-snapshot-repositories.md`](F05-snapshot-repositories.md), written on
  2026-09-30 independently from:
  - the Sparkles code and specs (CI and F06 in full; C09 §§3–4, 7, 9);
  - Elasticsearch and Kibana public user documentation on snapshot and restore, as
    evidence of desired behaviour only. Both products are source-available under
    non-permissive licenses, and no source code was read;
  - the `object_store` 0.14.2 docs (MIT/Apache-2.0), the AWS S3 User Guide on
    conditional writes and consistency, restic's design references (BSD-2-Clause, ideas
    only) and the `croner` 4.0.0 docs (MIT), all fetched;
  - `chrono-tz`, `lz4_flex`, `age`, BLAKE3, S3 limits, the borg chunker (BSD-3-Clause),
    `s3s-fs`, the MinIO license, RFCs 9110, 3339 and 9562 and the IANA time zone
    database, cited from general knowledge.
- **Implementation:** done (2026-09-30/10-01), from the spec, the implementation plan
  and the Sparkles code only. Where the plan and the spec differ, the plan wins. The
  implementation consists of:
  - the `sparkles-backup` crate, with the repository engine and policies as pure
    functions;
  - the engine's capture, lease, `reidentify` and `restoredFrom` support in `sparkles`;
  - the server's `backup` module, behind the default-on `backup` feature of
    `sparkles-server`. It holds the registry, task slots, HTTP routes, in-place restore
    and startup recovery, the policy scheduler and metrics;
  - the `sparkles repo` / `sparkles backup` commands, the UI's Backups page,
    `docs/API.md` ("Backup repositories") and `scripts/backup-bench.sh`.

  As the maintainer decided, the delivered Phase 1 is the spec's Phase 1 plus policies,
  GC and the full UI.
  - **Dependencies.** Versions come from `Cargo.lock`. Licenses come from each crate's
    `Cargo.toml` and were checked against its shipped license files.
    - `object_store` 0.14.2 (`MIT/Apache-2.0`), with `default-features = false` and the
      features `tokio`, `fs` and `aws`. These map to the crate features `fs` and `s3` of
      `sparkles-backup`, both on by default. `gcp` and `azure` stay behind the
      off-by-default `gcs` and `azure` features. The crate reuses the workspace's
      reqwest 0.13, hyper 1 and aws-lc-rs/rustls. The published crate ships only the
      Apache-2.0 text (`LICENSE.txt`) and the ASF `NOTICE.txt`. A binary distributed
      under the Apache-2.0 option must carry that NOTICE ("Apache Arrow Object Store,
      Copyright 2020-2026 The Apache Software Foundation"). This is not a blocker.
    - `croner` 4.0.0 (MIT; `LICENSE.md`), with `derive_builder` 0.20.2 (MIT OR
      Apache-2.0), `strum` 0.27.2 and `strum_macros` 0.27.2 (MIT). `derive_builder` uses
      `darling` 0.20.11 (MIT), a build-time proc macro. `croner` builds with chrono
      0.4.45, so the planned fallback parser was not needed.
    - `chrono-tz` 0.10.4 (MIT OR Apache-2.0), with `phf` / `phf_shared` 0.12.1 (MIT) and
      `siphasher` 1.0.4 (MIT OR Apache-2.0). The bundled IANA tz data is public domain.
      There is no `chrono-tz-build` step, because the 0.10 tables are pre-generated.
    - New through `object_store`:
      - `crc-fast` 1.10.0 (MIT OR Apache-2.0), pure Rust with no build script, with
        `digest` 0.10.7 and `crypto-common` 0.1.7 (MIT OR Apache-2.0), `generic-array`
        0.14.7 (MIT) and `spin` 0.10.1 (MIT);
      - `humantime` 2.4.0 (MIT OR Apache-2.0);
      - `itertools` 0.15.0 (MIT OR Apache-2.0), a second version next to 0.14.0;
      - `nix` 0.31.3 (MIT);
      - `quick-xml` 0.41.0 (MIT), next to 0.37.5;
      - `wasm-streams` 0.5.0 (MIT OR Apache-2.0), for wasm targets only and never built
        here.
    - Reused, already in the workspace: `sha2`, `lz4_flex`, `futures`, `bytes`,
      `async-trait`, `percent-encoding`, `tokio`, `uuid`, `chrono`, `serde_json`, `toml`.
    - Nothing is non-permissive. Every new crate is MIT, Apache-2.0 or both. MinIO
      (AGPL-3.0) is only an external test process, and the S3 tests run only with
      `SPARKLES_TEST_S3_ENDPOINT`. `age`, for Phase 3 encryption, is not a dependency
      yet.
  - **Deviations from the spec.** The plan and the implementation made these decisions:
    - Leases are keyed by generation, not by commit (`Lease { generation, label }`).
      After a compaction, the new generation's base covers the captured commit, so a
      lease by commit would not have kept the old generation. History GC keeps a leased
      generation unconditionally and shows it as `backup:<name>`.
    - `validation.json` and `validation-shapes.ttl` are backed up as meta files, and the
      restore path grammar accepts them. The spec's file list omitted them, and clones
      copy them as well.
    - Repositories registered through the API are restricted beyond the spec:
      - S3 credentials are named only (`{"source": "named", "name": …}`) and resolved
        from the config file's `[credentials.<name>]` tables. The caller can never
        choose `env`, `file` or `default`.
      - S3 endpoints, and every address their host resolves to, are under the server's
        outbound policy. A custom DNS resolver pins connections to the checked
        addresses.
      - `fs` paths must lie under `[api] fs_roots` when it is set, and outside the
        directories of the server's config files.
      - `gcs` and `azure` repositories come from the config file only.
      - Endpoint URLs refuse userinfo, queries and fragments.
    - Blob header byte 5 is the codec (0 raw, 1 LZ4, 2 zstd reserved), and byte 6 is
      the encryption (0 none). The format is still 1.
    - During an in-place restore, a router layer answers requests for the dataset with
      `503 dataset-restoring` and `Retry-After: 5`. The spec's check in `dataset()`
      could not guarantee "never 404".
    - Server-wide tasks have `dataset: ""` and are listed for `server-admin` only.
    - `verified` comes from a server-local `<data>/backup/verify.json`, and
      `Repository.lastGc` from `gc/last.json`.
    - `<data>/sparkles-server.lock` allows one server per data directory and guards
      `sparkles backup restore --data`.
    - Schedules use croner in naive local time with a DST mapping written for Sparkles.
      Skipped times run after the gap, and repeated times run once. `every <duration>`
      is aligned to the epoch in UTC.
    - At most 4 tasks wait per `--backup-max-tasks` slot. Beyond that, requests get
      `503 too-many-tasks`.
    - Some parts are not built as specified:
      - `507 insufficient-storage` is defined, but no restore checks free space.
      - `sparkles_backup_object_requests_total` counts only the successful requests
        that verification and GC reports carry (`result="ok"`), not every request with
        `ok|error`.
      - In-memory datasets answer `501 backup-unsupported`.
      - `sparkles backup policy run` (offline) is refused until Phase 2.
- **Rejected** (spec §8):
  - N-Quads dumps as the backup format;
  - a tar of the database directory per backup;
  - mirroring Elasticsearch's repository layout, blob naming or REST paths;
  - "snapshots" as the name, or `/$/snapshots/…` paths (F06 uses them);
  - `/$/repositories/{repo}/backups/{name}` for per-dataset operations;
  - content-defined chunking and pack files in Phase 1;
  - multipart uploads;
  - BLAKE3 (SHA-256 is already in the workspace);
  - a mutable repository index object;
  - client-clock lock staleness;
  - always keeping, or always replacing, the dataset id on restore;
  - backing up the Tantivy index;
  - blocking writes for the whole backup;
  - filesystem snapshots or rsync;
  - inline secrets in the API or UI in Phase 1.

## Formatter

- **Spec:** [`X02-formatter.md`](X02-formatter.md), written on 2026-09-30 independently
  as internal engineering. Its sources are:
  - the W3C and IETF specifications: SPARQL 1.1 and 1.2 Query, SPARQL 1.1 Update and
    Results JSON, RDF 1.1 and 1.2 Turtle, TriG, N-Triples, N-Quads and Concepts,
    RDFC-1.0, JSON-LD 1.1, RFC 8259 and BCP 47;
  - the pretty-printing papers of Wadler, Lindig and Oppen, and the Prettier docs (MIT);
  - the Oxc/oxfmt docs, `Cargo.toml` files and npm and crates.io metadata (MIT);
  - prior art through public documentation and licenses only: Apache Jena, rdflib,
    turtlefmt, atextor/turtle-formatter, sparqling/sparql-formatter, Qlue-ls, TopBraid's
    published Turtle files, and the tree-sitter SPARQL and Turtle grammars (checked for
    RDF 1.2 constructs only);
  - the Sparkles code, spargebra 0.4.7, oxttl 0.2.4 and oxrdf 0.3.4.

  No code was read or copied from any GPL or LGPL project.
- **Implementation, slice 1** (2026-09-30): SPARQL queries and updates, `sparkles fmt`,
  `POST /$/format` and the UI's Format button. It was built from the spec, the slice plan
  and the Sparkles code only.
  - **Design sources:**
    - Philip Wadler, "A prettier printer" (1998/2003), for the document algebra (text,
      line, group, nest) and the fits-then-break rule;
    - Prettier's printer algorithm as its public docs and `commands.md` describe it (MIT):
      `group`, `indent`, `line`/`softline`/`hardline`, `ifBreak` with a group id,
      `lineSuffix`, `breakParent`, `cursorOffset`, `--check`/`--list-different`/`--write`
      and their exit codes, ignore files and `prettier-ignore`;
    - rust-analyzer's syntax-tree approach, from its public architecture and design docs
      (MIT OR Apache-2.0). It uses a lossless token stream and a hand-written
      recursive-descent parser with markers (`start`/`complete`/`precede`), and keeps
      trivia by token adjacency;
    - the W3C SPARQL 1.1 and 1.2 grammars (EBNF, terminals, keyword spellings), with
      spargebra 0.4.7 (MIT OR Apache-2.0, already a dependency) as the reference parser and
      the source of the algebra the safety check compares.
  - **New dependencies** (all permissive; `Cargo.lock` and `THIRD_PARTY_LICENSES.md`
    regenerated):
    - `unicode-width` 0.2.2 (MIT OR Apache-2.0): display widths in the printer
      (`sparkles-fmt`);
    - `ignore` 0.4.33 (Unlicense OR MIT): directory walks with gitignore rules and
      `.sparklesfmtignore` (server, feature `fmt`);
    - `similar` 2.7.0 (Apache-2.0): `--diff` unified diffs (server, feature `fmt`). The
      spec named version 3, but the lock had 2;
    - `proptest` 1.11.0 (MIT OR Apache-2.0): property tests, dev only (`sparkles-fmt`);
    - `toml` 1.1.6 (MIT OR Apache-2.0, already an optional dependency for auth and backup):
      `.sparklesfmt.toml`, now also enabled by feature `fmt`;
    - reused: `thiserror` 2.0.21 (MIT OR Apache-2.0), `rayon` and `tempfile` (parallel
      files and atomic `--write`), `sha2` (the input hash in refusal logs).
  - **Test-only switch:** the `fault-injection` feature of `sparkles-fmt`. Only
    dev-dependencies enable it, in its own tests and in `sparkles-server`'s. With
    `SPARKLES_FMT_FAULT=drop-token`, the printer drops a token, so the tests can show
    that the CLI and the endpoint refuse such output.
- **Later slices** (2026-10-01): Turtle, TriG, N-Triples, N-Quads and JSON-LD,
  `sparkles lsp`, the optional WebAssembly build for the UI, and streaming Turtle and
  TriG. They were built from the spec, the plan for the remaining slices and the
  Sparkles code only.
  - **New dependencies** (all permissive; versions from `Cargo.lock`):
    - `json-event-parser` 0.2.3 (MIT OR Apache-2.0, from the Oxigraph developers): the JSON
      tokenizer of the JSON-LD formatter (`sparkles-fmt`);
    - `lsp-server` 0.10.0 (MIT OR Apache-2.0) and `lsp-types` 0.97.0 (MIT): `sparkles lsp`
      (server, feature `fmt`);
    - `wasm-bindgen` 0.2.129 (MIT OR Apache-2.0) and its macro and support crates: the
      `sparkles-fmt-wasm` crate, built only by its own task, never linked into the server.
- **Rejected** (spec §12):
  - re-serializing through spargebra `Display` or the `oxttl` serializers;
  - tree-sitter grammars;
  - embedding oxfmt, or shelling out to oxfmt or Prettier, for JSON-LD;
  - the `pretty` crate;
  - aligning predicate columns, prefix IRIs or `VALUES` columns;
  - many style options;
  - formatting RDF/XML;
  - formatting SPARQL inside SHACL literals;
  - canonicalizing literal lexical forms.

## Linter

- **Spec:** [`X04-linter.md`](X04-linter.md), written on 2026-10-02 as internal
  engineering, the `sparkles lint` that [X02](X02-formatter.md) deferred. Its sources are:
  - the W3C SPARQL 1.1 and 1.2 Query specifications (variable scope, grouping, and the
    algebra of FILTER, OPTIONAL and MINUS), RDF 1.1 and 1.2 Concepts, XML Schema 1.1
    Part 2, BCP 47 (RFC 5646) and the IANA language subtag registry, the OWL 2 mapping to
    RDF, and the Language Server Protocol 3.17;
  - Apache Jena's `ARQConstants` and `MappedLoader` (Apache-2.0), read for the old
    `jena.hpl.hp.com` namespaces and Jena's warning about them. No code was copied;
  - the public documentation of ESLint and Ruff (MIT) for the command's conventions:
    severities per rule, `--fix` for safe fixes only, and the exit statuses;
  - the Sparkles code: the formatter's lexer, syntax tree, prefix scopes, reference
    parses and equivalence checks.

  No code was read or copied from another linter.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. **Dependencies:**
  `oxsdatatypes` 0.2 (MIT OR Apache-2.0), already a dependency of `sparkles`, became a
  dependency of `sparkles-fmt` for the XML Schema date, time and duration forms.
- **Rejected** (spec §6): rules over the SPARQL algebra, a config file of its own, and
  fixes that change a document's meaning.

## TriX

- **Design source:** Apache Jena's `ReaderTriX`, `WriterTriX`, `StreamWriterTriX` and
  `TriX` classes and their tests `TestTriXReader`, `TestTriXBad` and `TestTriXWriter`
  (Apache-2.0), read for behaviour: the element names of the HPL report and the W3C DTD,
  the unnamed graph as the default graph, plain and typed literals, `rdf:XMLLiteral`
  content, `<qname>`, triple terms, and the writer's layout. The format itself is
  Carroll and Stickler's "TriX: RDF Triples in XML" (HP Labs, HPL-2004-56). No code was
  copied.
- **Test files:** Jena's `jena-arq/testing/RIOT/Lang/TriX` directory is copied unchanged
  into `testsuite/trix/jena`, with Jena's `LICENSE` (as `LICENSE-APACHE`) and `NOTICE`, as
  the Apache License 2.0 allows. The unit tests of `sparkles::trix` run Jena's cases
  against them.
- **Implementation** (2026-10-02): `sparkles::trix` in the core crate, used by the
  server's Jena formats, `sparkles load` and `sparkles convert`, and SPARQL Update's
  `LOAD`. **Dependencies:** `quick-xml` 0.37 (MIT), already in the tree through
  `oxrdfxml` and `sparesults`, became a direct dependency of `sparkles`.

## Patched spargebra (vendored)

- **Adopted:** `spargebra` 0.4.7 from crates.io (2026-09-30). Its `Cargo.toml` gives the
  license as MIT OR Apache-2.0, and the copyright belongs to the Oxigraph developers. The
  crate is copied to `vendor/spargebra` and used through `[patch.crates-io]` in the
  workspace `Cargo.toml`.
- **Changes** (`vendor/spargebra/PATCHED.md`): written against 0.4.7's rust-peg grammar
  from the W3C SPARQL 1.1 and 1.2 Query grammars and the W3C test suite. None were
  ported from upstream.
  - left-associative `+ -` and `* /`;
  - case-insensitive `true` / `false`;
  - longest match for `<` against `IRIREF`;
  - the FILTER scope in OPTIONAL;
  - SELECT expressions see earlier aliases;
  - no nested aggregates;
  - triple-term subjects restricted per the SPARQL 1.2 grammar rules [123] and [138];
  - Jena ARQ's syntax extensions behind one switch, `with_arq_syntax`: the aggregate
    keywords, `LATERAL` (spargebra's own `sep-0006` code), path ranges and CONSTRUCT
    templates with `GRAPH`, written from ARQ's grammar for spec G06.
- **Notes:** like the published crate, the copy has no `LICENSE` files. The license is
  the `Cargo.toml` field. The copy will be dropped once a spargebra release has these
  fixes.
- **Rejected:** waiting for an upstream release, and porting the rewritten parser of
  Oxigraph's development version.

## GeoSPARQL

- **Spec:** [`G01-geosparql.md`](G01-geosparql.md). Its sources are:
  - the OGC GeoSPARQL 1.1 standard (OGC document license);
  - Apache Jena's `jena-geosparql` (Apache-2.0), read for behaviour, the `spatial:` and
    `spatialF:` surface and its divergences. No code was copied;
  - Oxigraph's `spargeo` (Apache-2.0/MIT) and QLever's spatial support (Apache-2.0),
    both read;
  - the Sparkles code.
- **Implementation, Phase 1** (2026-10-01): the `geo` feature of `sparkles`, which is on
  in the server. It is built on three crates:
  - `geo` 0.33.1 (MIT OR Apache-2.0), with default features off, renamed `georust`;
  - `geo-index` 0.4.0 (MIT OR Apache-2.0);
  - `geographiclib-rs` 0.2.7 (MIT), a port of Karney's GeographicLib.

  `wkt` and `geojson` were tried and dropped. The new transitive crates (`i_overlay`,
  `robust`, `rstar`, `geo-traits`, `geo-types`, `float_next_after`, `heapless`,
  `accurate`, …) are all MIT and/or Apache-2.0, and there is no `earcutr` or `spade`.
  `THIRD_PARTY_LICENSES.md` lists them.
  - The WKT and GeoJSON readers and writers are hand-written from the GeoSPARQL 1.1 and
    Simple Features grammars and RFC 7946. The `wkt` crate gives no error offsets and
    has no TRIANGLE/TIN. The CRS and unit tables come from the OGC, EPSG and QUDT
    registries.
  - Topology uses DE-9IM (`geo`'s relate). Geodesic distance, area and buffers use
    GeographicLib (Karney 2013), and GeographicLib's published values serve as test
    oracles.
  - The spatial index (packed R-tree base, overlay and tail) and the planner and
    executor are written from the spec and the Sparkles store and planner code.
  - **Test data and tools:** the GeoSPARQL Compliance Benchmark is GPL-2.0 and is never
    vendored. `scripts/geosparql-benchmark.sh` fetches it at test time into `target/`
    at pinned commit 879e0746, and only with `SPARKLES_ALLOW_GPL_BENCHMARK=1`. Its README
    and file names were consulted to write the runner. Its code was not.
- **Phase 2** (2026-10-01): spatial joins, nearest neighbours, query rewrite, hulls and
  aggregates, conversion and the UI's maps. Built from the spec, the Phase 2 plan and the
  Sparkles code.
  - **UI dependencies:** `maplibre-gl` 6.11.2 (BSD-3-Clause) and its `@maplibre/*` packages
    (ISC, MIT, MIT OR Apache-2.0), listed in `THIRD_PARTY_LICENSES-UI.md`.
  - **Data:** the bundled basemap is Natural Earth 1:110m, release 5.1.2 (public domain),
    reduced by `scripts/basemap.mjs` (`ui/src/lib/basemap/README.md`).
  - The GeoSPARQL vocabulary axioms behind `--vocab geosparql` are written by hand from
    the standard. No OGC files are vendored.
- **Phase 3** (2026-10-02): geometry types from literals, variable `spatial:` arguments,
  the cell grid for spatial tests, GML and KML literals, and CRSs from proj4 definitions.
  Built from the spec and the Sparkles code.
  - The GML and KML readers and writers are written from the GML 3.2 Simple Features
    profile and KML 2.2. They read XML with `quick-xml` 0.37 (MIT), which the RDF/XML
    parser already used. The GML class hierarchy follows the substitution groups of the
    GML 3.2 schemas, and no schema file is vendored. The tests use the examples printed in
    the GeoSPARQL 1.1 standard (OGC 22-047r1).
  - The cell grid takes only the idea of QLever's grid approximation of regions.
  - **Dependencies:** `proj4rs` 0.2.0 (MIT OR Apache-2.0), with its default features off,
    behind the `geo-proj4` feature that the server's `geo` feature turns on. Its only new
    dependency is `thiserror` 2. `crs-definitions` 0.5.0 (CC0-1.0, derived from the EPSG
    dataset) is linked only by the opt-in `geo-epsg` feature, because the EPSG terms of
    use restrict redistribution. Sparkles has no `deny.toml`. The license check is
    `scripts/third-party-licenses.py`, and both licenses are permissive.

## ShEx validation

- **Spec:** [`G02-shex.md`](G02-shex.md), written on 2026-09-30 independently from:
  - the Sparkles code and spec C10;
  - the ShEx 2.1 Final Community Group Report and the ShapeMap draft from shex.io, both
    fetched. Both are under the W3C Software and Document License, and the shape-map
    repository is MIT;
  - the shexTest suite (W3C Software and Document License), upstream and as vendored in
    Apache Jena;
  - Apache Jena's `jena-shex`, `jena-cmds` and `jena-fuseki2` sources (Apache-2.0), read
    for behaviour and CLI surface. No code was copied;
  - rudof 0.3.24 (MIT OR Apache-2.0), inspected for licensing, dependency weight, API
    and conformance. It is not a dependency;
  - papers by Staworko et al. (ICDT 2015), Boneva, Labra Gayo and Prud'hommeaux (ISWC
    2017) and Labra Gayo et al. (2015, RBE derivatives).
- **Implementation, Phase 1** (2026-10-01): from the spec, the plan and the Sparkles
  code.
  - The conformance harness (`crates/sparkles-shex/tests/shextest.rs`) reads the
    shexTest manifests itself, with oxttl for Turtle and serde_json for JSON-LD. Jena's
    `ShexTests.java` and `ShexValidationTest.java` (Apache-2.0) were read to learn how
    the suite is run: base IRIs, the start-focus and map forms, extension results and
    its exclusions. No code was copied.
  - The server endpoint (`http/shex.rs`) and `sparkles shex` (`shex_cmd.rs`) follow the
    `/{ds}/shacl` handler and `sparkles shacl` in the Sparkles code.
  - The ShExC lexer and parser (`shexc/`) are hand-written from the ShEx 2.1 grammar,
    without a parser generator. The suite's ShExJ files were the reference for the
    parsed form.
  - The ShExJ reader (`shexj.rs`) follows the ShExJ section of the ShEx 2.1 report.
  - Schema checks and compilation follow the report's well-formedness and stratification
    rules (Boneva et al. 2017 for stratified negation).
  - The matcher decides a neighbourhood partition by iteration-count intervals per
    group, not by RBE derivatives. The brute-force partition enumerator in its tests
    follows the report's partition definition literally. Typing is a per-stratum
    greatest fixed point (Staworko et al. 2015; Boneva et al. 2017).
  - Node constraints reuse `sparkles::xsd` for lexical forms and value comparison.
    Facet semantics follow the report and its errata.
  - Shape maps follow the ShapeMap draft. Jena's compact-map extensions (BASE/PREFIX,
    optional commas, trailing `.`) were learned from reading `jena-shex`. No code was
    copied.
  - Imports and EXTERNAL resolution follow the report. http(s) imports go through
    `sparkles::outbound`.
  - **Dependencies:** none new. `sparkles-shex` uses workspace crates only, and
    `smallvec` and `oxilangtag` were already in the lock.
  - **Test data:** shexTest is read from the Jena checkout or `SPARKLES_SHEX_TESTS`,
    not vendored.
  - **Development-only:** the rudof CLI as a differential oracle (`bench:shex
    --compare-rudof`), never linked.
- **Rejected** (spec §12):
  - depending on rudof, or vendoring its parser crate;
  - porting Jena's partition-enumeration validator;
  - goal-directed recursive validation with hypothesis stacks;
  - translating ShEx to SHACL or SPARQL;
  - RBE derivatives as the matcher. The spec preferred them, but the implementation
    adopted the interval method because it is exact and linear, and an oracle checks
    it;
  - ShEx 2.2 in Phase 1;
  - executing semantic actions other than the Test extension;
  - an RDF report format;
  - ShExC literals in named graphs;
  - SHACL and ShEx guards together in Phase 2;
  - a `peg` grammar;
  - treating budget overruns as nonconformant.

## SHACL Compact Syntax and list constraints

- **Spec:** [`G03-shaclc.md`](G03-shaclc.md), written on 2026-10-02 independently from:
  - the Sparkles code and specs C02, C10 and C11;
  - the SHACL 1.2 Core and SHACL 1.2 Compact Syntax editor's drafts and the SHACL 1.0
    Compact Syntax Working Group Note, fetched from the Working Group's pages. They are
    under the W3C Software and Document License;
  - the SHACL Compact Syntax test pairs, as copied into Apache Jena, and the SHACL 1.2
    list constraint tests of the Working Group's test suite;
  - Apache Jena's `jena-shacl` sources (Apache-2.0): the SHACLC grammar, reader, writer,
    list constraints and tests, read for behaviour. No code was copied.
- **Implementation** (2026-10-02): from the spec and the Sparkles code.
  - The lexer and parser (`compact/lex.rs`, `compact/read.rs`) are hand-written from the
    grammar and production rules of the Compact Syntax drafts. Jena's JavaCC grammar and
    `ShaclCompactParser.java` were read for the extensions and the datatype rule. No
    code was copied.
  - The writer (`compact/write.rs`) is written from spec §5. It does not follow Jena's
    writer, which works from the parsed shapes.
  - The list constraints follow the SHACL 1.2 Core draft's textual definitions and its
    definition of a SHACL list. Jena's `List*` constraint classes were read for behaviour.
  - **Dependencies:** none new. `sparkles-shacl` now uses `oxiri`, already in the
    workspace.
  - **Test data:** the eight SHACL 1.2 list tests are vendored in
    `crates/sparkles-shacl/tests/shacl12/` under the W3C Software and Document License,
    with a README. The SHACLC test pairs and Jena's syntax tests are read from the Jena
    checkout, not vendored.
- **Rejected** (spec §10):
  - writing SHACLC from the parsed shapes model;
  - skipping what cannot be written;
  - storing inline SHACLC as written;
  - a parser generator;
  - SHACLC as a general RDF syntax.

## Python bindings

- **Spec:** [`P01-python-bindings.md`](P01-python-bindings.md), written on 2026-10-02
  independently from:
  - the Sparkles code;
  - the PyO3 0.29 user guide and API documentation, and the maturin 1.x user guide;
  - PEPs 384, 517, 561, 599, 600 and 639, and the CPython release schedule;
  - pyoxigraph's documentation (MIT OR Apache-2.0), read for the names and signatures of
    its store, term, `parse`, `serialize` and `RdfFormat` API. Its source was not read;
  - rdflib's documentation (BSD-3-Clause), read for `rdflib.term` and
    `Literal.toPython`.
- **Implementation, Phase 1** (2026-10-02): from the spec and the Sparkles code. No code
  was copied from pyoxigraph or rdflib.
  - `crates/sparkles-py` is a cdylib crate in its own cargo workspace. The term classes,
    result iterators, transactions and error mapping are written against PyO3's class and
    function macros. The Python exceptions are defined in `sparkles/_errors.py`.
  - Transactions run the engine's closure-based `Dataset::transaction` on a worker
    thread, which owns the writer lock's guard, and receive operations over a channel.
  - `Dataset::quads` and `QuadIter` were added to `crates/sparkles` for the streaming
    `quads_for_pattern`. They read the index in batches with the existing
    `Snapshot::scan_between`.
  - **Dependencies:** `pyo3` 0.29.3 (MIT OR Apache-2.0) with the `abi3-py310` feature,
    with `pyo3-ffi`, `pyo3-macros` and `pyo3-macros-backend` 0.29.3 (MIT OR Apache-2.0)
    linked or expanded into the extension, and `pyo3-build-config` 0.29.3 (MIT OR
    Apache-2.0), `target-lexicon` 0.13.5 (Apache-2.0 WITH LLVM-exception) and `heck`
    0.5.0 (MIT OR Apache-2.0) at build time only. None of them is linked into the
    `sparkles` binary. `crates/sparkles-py/THIRD_PARTY_LICENSES.md` lists the crates the
    wheel links.
  - **Build and test tools, not shipped:** maturin 1.15.0 (MIT OR Apache-2.0), pinned in
    `mise.toml` for `py:build` and taken from nixpkgs in the flake; pytest (MIT), mypy and
    its `stubtest` (MIT) and rdflib (BSD-3-Clause) for the test suite, from the dev shell
    or nixpkgs.
- **Rejected** (spec §2.2 and §11):
  - a workspace member left out of `default-members`, which would make every workspace
    lint and test build compile PyO3;
  - an rdflib `Store` plugin in Phase 1;
  - a binding-side table that keeps blank-node labels across writes;
  - holding the store's write transaction in the Python object, which needs its guard on
    one thread.

## Rust client

- **Spec:** [`P02-rust-client.md`](P02-rust-client.md), written on 2026-10-02
  independently from:
  - the Sparkles code and `docs/API.md`, and the OpenAPI description of X03
    (`docs/openapi.json`);
  - the W3C SPARQL 1.1 Protocol, Graph Store HTTP Protocol and Query Results formats;
  - RFC 9110, RFC 7617, RFC 6750, RFC 8187 and draft-ietf-httpapi-ratelimit-headers;
  - Apache Jena's documentation of `RDFConnection` and `RDFLink` (Apache-2.0), read for
    the operations and their names. Its source was not read;
  - Oxigraph's Rust API documentation and pyoxigraph's documentation (MIT OR
    Apache-2.0), read for `QueryResults` and streaming solutions;
  - the documentation of progenitor (MPL-2.0) and openapi-generator (Apache-2.0), read to
    decide against generating the client;
  - the docs.rs documentation of reqwest, tokio, tokio-util, sparesults and oxrdfio.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. No code was
  copied from Jena, Oxigraph or a generator. The URL normalization and the credentials
  file moved from the server crate into the client, which the CLI now uses.
  - **Dependencies:** none new to the lock file. The crate uses `reqwest` 0.13.5 (MIT OR
    Apache-2.0) with its `stream` and `multipart` features newly enabled, which bring in
    `mime_guess` 2.0.5 (MIT), already in the tree. It also uses `tokio` 1.53.1 (MIT),
    `tokio-util` 0.7.19 (MIT), `futures-util` 0.3.34 (MIT OR Apache-2.0), `httpdate`
    1.0.3 (MIT OR Apache-2.0), `toml` 1.1.6 (MIT OR Apache-2.0), and `oxrdfio` and
    `sparesults` (MIT OR Apache-2.0) with their `async-tokio` features newly enabled.
    The `sparkles` binary links the client through the server's `auth` feature, and
    `THIRD_PARTY_LICENSES.md` did not change.
  - **Test only:** `axum` 0.8.9 (MIT) for the mock server of the retry tests, already a
    dependency of the server.
- **Rejected** (spec §2 and §6):
  - a client generated by progenitor, which reads OpenAPI 3.0 only;
  - a client generated by openapi-generator, which cannot stream results;
  - moving the CLI's `--server`, `rsparql` and `rupdate` requests onto the client.

## Automatic compaction

- **Spec:** [`C13-automatic-compaction.md`](C13-automatic-compaction.md), written on
  2026-10-02 independently from:
  - the Sparkles code and the [F05](F05-snapshot-repositories.md),
    [F06](F06-snapshots-and-point-in-time.md) and
    [C01](C01-observability-and-budgets.md) specs;
  - QLever's `--rebuild-index-strategy` option and the comments of its `DeltaTriples.h`,
    `IndexRebuilder.h` and `IndexSwap.h` headers (Apache-2.0), read for behaviour;
  - LSM compaction in LevelDB and RocksDB, PostgreSQL's autovacuum thresholds and Jena
    TDB2's compaction, cited from their public documentation and general knowledge.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. No code was copied
  from QLever. **Dependencies:** none new. `libc`'s `setpriority`, already a dependency of
  the engine on Unix, lowers the build threads' priority.
- **Rejected** (spec §8):
  - holding the writer lock for the whole automatic build;
  - a thread per dataset;
  - carrying the net delta instead of each commit;
  - refusing writes during a build;
  - a size-only or ratio-only trigger;
  - compacting every idle period whatever the delta's size.

## Branches and merges

- **Spec:** [`F09-branches-and-merges.md`](F09-branches-and-merges.md), written on
  2026-10-02 independently from:
  - the Sparkles code and the [C06](C06-clone-to-sandbox.md),
    [F06](F06-snapshots-and-point-in-time.md), [CI](CI-commit-identity.md),
    [C13](C13-automatic-compaction.md), [F05](F05-snapshot-repositories.md) and
    [C12](C12-graph-access-control.md) specs;
  - Git's model of branches, merge bases, three-way merges and two-parent merge commits,
    from its documentation, cited from general knowledge;
  - the public documentation of Dolt (merges and conflict tables) and Project Nessie
    (expected hashes), both fetched, and of lakeFS (merge strategies) and TerminusDB
    (delta layers and rebase merges), read through searches;
  - Datomic's documentation of `with`, cited from general knowledge;
  - the literature on three-way merges (diff3), replicated sets and version vectors, and
    the Quit Store and R43ples papers on versioned RDF, cited from general knowledge;
  - RDF 1.2 Concepts, RDFC-1.0 as a test oracle, the SPARQL 1.1 Protocol and Jena's RDF
    Patch documentation.

  Jena and Oxigraph have no branches, so neither was a source.
- **Implementation:** not started. **Dependencies:** none planned.
- **Rejected** (spec §8):
  - a branch as a full clone;
  - branches inside one store with one tagged log;
  - a content-addressed index;
  - rebase as the merge model;
  - quad-level conflicts as the default, and conflicts from the ontology alone;
  - conflicts kept on the server until resolved;
  - relabelling blank nodes at merge time, and merging isomorphic blank-node structures;
  - hard links or reflinks to share generation files;
  - branches as separate datasets in the registry;
  - moving refs on fast-forward;
  - a branch syntax inside `at`.

## Command-line tools and IRI and language-tag checks

- **Spec:** [`G05-command-line-tools.md`](G05-command-line-tools.md), written on
  2026-10-02 independently from:
  - the Sparkles code;
  - Apache Jena's `jena-cmds` (`riot`, `CmdLangParse`, `ModLangParse`, `ModLangOutput`,
    `qparse`, `uparse`, `rdfdiff`, `rdfcompare`, `iri`, `rsparql`, `rupdate`, `rset`),
    `jena-langtag` (`CmdLangTag`) and `jena-iri3986` (`Issue`) sources (Apache-2.0),
    read for behaviour, flags, output and the list of IRI issues. No code was copied;
  - RFC 3986, RFC 3987, RFC 9110 §4.2, RFC 8141, RFC 9562, RFC 3061, RFC 8089, W3C DID
    Core 1.0 §3.1, the IANA URI scheme registry, BCP 47 (RFC 5646), RDF 1.1 and 1.2
    Concepts, RDFC-1.0, the SPARQL 1.1 Protocol and the SPARQL result formats.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. The IRI and
  language-tag rules, dot-segment removal and the per-group isomorphism check are written
  from the RFCs. **Dependencies:** none new. `sparesults` 0.3 (MIT OR Apache-2.0), already
  a dependency of `sparkles`, became a direct dependency of `sparkles-server`.
- **Rejected** (spec §8):
  - a plain endpoint URL in `query --server`, which would send saved tokens to other
    servers;
  - canonicalizing whole datasets for the diff of `compare`;
  - checking terms inside the loader;
  - a subtag registry for `langtag`.

## CSV and TSV imports

- **Spec:** [`C05-tabular-imports.md`](C05-tabular-imports.md), written on 2026-10-02
  independently from:
  - the W3C Recommendations "Model for Tabular Data and Metadata on the Web", "Metadata
    Vocabulary for Tabular Data" and "Generating RDF from Tabular Data on the Web", cited
    from working knowledge;
  - RFC 4180, RFC 6570, RFC 7111 and the IANA registration of
    `text/tab-separated-values`;
  - Tarql's public documentation (Apache-2.0), for CONSTRUCT templates over rows. Its
    source was not read;
  - the W3C R2RML Recommendation and the RML and YARRRML specifications, at the level of
    their overviews, for the rejected alternative;
  - the Sparkles code and specs C01 and C15.
- **Implementation:** from the spec plus Sparkles code only (2026-10-02). No CSVW
  processor's or Tarql's code was read or copied. The W3C CSVW test suite was not
  available offline, so the tests were written from the spec's examples.
  - `sparkles::tabular` holds the converter: the dialect and the record reader on the
    `csv` crate, the CSVW metadata subset, the datatypes and their formats, an RFC 6570
    URI template expander, the minimal-mode triples and the CONSTRUCT templates, which
    run on the engine's `sparql::execute_query` with a `VALUES` block placed at the start
    of the WHERE clause.
  - `sparkles-server` has the `load` options and `sparkles csv` in `csv_cmd`, and the
    upload's conversion in `http::tabular`.
  - **Dependencies:** `csv` 1.4.0 and `csv-core` 0.1.13 (Unlicense OR MIT), linked into
    the `sparkles` binary and the Python wheel.
- **Rejected** (spec §8):
  - RML, R2RML and YARRRML mappings, deferred until users bring existing mappings;
  - CSVW's standard mode;
  - type inference for the default mapping;
  - a loader `Source` that holds the converter instead of a temporary N-Triples file;
  - a SPARQL evaluation per row for templates;
  - templates that read the target dataset;
  - CSV bodies in the Graph Store protocol.

## Applying RDF Patch, replication and read replicas

- **Spec:** [`F10-replication.md`](F10-replication.md), written on 2026-10-02
  independently from:
  - the Sparkles code and the [CI](CI-commit-identity.md),
    [F06](F06-snapshots-and-point-in-time.md), [C13](C13-automatic-compaction.md),
    [F05](F05-snapshot-repositories.md), [C09](C09-dataset-access-control.md) and
    [C12](C12-graph-access-control.md) specs;
  - Apache Jena's `jena-rdfpatch`, `jena-cmds` (`rdfpatch`) and Fuseki (`PatchApply`,
    `FusekiConfig`, `OperationRegistry`) sources and `GraphView` (Apache-2.0), read for
    behaviour. No code was copied;
  - the RDF Patch documentation of Apache Jena and the RDF Delta documentation (patch
    logs, `id` and `prev`), fetched;
  - the PostgreSQL documentation of log-shipping standby servers and hot standby, the
    LiteFS documentation and Oxigraph 0.3's `Store` documentation, fetched for
    behaviour;
  - Litestream's design, the Raft paper and the etcd documentation, PostgreSQL timelines
    and RFCs 9110, 9530, 9651 and 6648, cited from general knowledge.
- **Implementation:** Phase 1 from the spec plus Sparkles code only (2026-10-02), with no
  new dependency. The patch reader was written from the row grammar and the RDF Thrift
  schema (`BinaryRDF.thrift`), not from Jena's reader. The tests port the patches of
  `jena-rdfpatch`'s `TestPatchIO_Text`, `AbstractTestPatchIO` and
  `testing/files/syntax-1.rdfp` (Apache-2.0), and the fixtures in
  `crates/sparkles/tests/patch` were written by Jena 6.2.0's patch writers. Phases 2 and 3
  are deferred.
- **Rejected** (spec §10):
  - physical replication of WAL bytes and generation files;
  - the public change feed as the replication protocol;
  - shipping SPARQL Update text;
  - push from the primary;
  - followers that read the primary's files;
  - consensus (Raft) in Phases 1 and 2;
  - keeping the dataset id on promotion;
  - replicating every compaction as a generation copy;
  - applying bulk commits from their changes by default;
  - redirecting writes from a replica to the primary;
  - Jena's blank-node labels as identities;
  - one commit per `TX … TC` block as the default.

## Cypher read subset over RDF

- **Spec:** [`F01-cypher.md`](F01-cypher.md), written on 2026-10-02 independently from:
  - openCypher 9 (the Cypher Query Language Reference and grammar) and the openCypher
    TCK (Apache-2.0), whose feature directories and license were checked on 2026-10-02;
  - public summaries of ISO/IEC 39075:2024 (GQL) and ISO/IEC 9075-16:2023 (SQL/PGQ),
    and Deutsch et al., "Graph Pattern Matching in GQL and SQL/PGQ" (SIGMOD 2022);
  - the Neo4j Cypher manual and the Neo4j Query API documentation;
  - Amazon Neptune's openCypher compliance page, the Neptune Analytics guide to RDF
    data, an AWS blog post on RDF and openCypher, Schmidt et al., "openCypher over RDF:
    Connecting Two Worlds" (ISWC 2024 demo), and Lassila et al., "The OneGraph vision"
    (Semantic Web, 2023);
  - the Kùzu documentation of RDF graphs, the neosemantics documentation, Ontotext
    GraphDB's graph path search documentation, and Stardog community forum answers on
    its TinkerPop support;
  - W3C RDF 1.2 Concepts, Turtle 1.2 and SPARQL 1.2 Query drafts, the RDF-star
    Community Group report (2021), and Hartig, "Reconciliation of RDF* and Property
    Graphs" (arXiv:1409.3288);
  - Francis et al., "Cypher: An Evolving Query Language for Property Graphs" (SIGMOD
    2018), Angles, Thakkar and Tomaszuk, "Mapping RDF Databases to Property Graph
    Databases" (IEEE Access, 2020), Steer et al., "Cytosm" (GRADES 2017), Thakkar et
    al., "Gremlinator" (GRADES-NDA 2018), and Zhao et al., "S2CTrans" (2023);
  - the Sparkles code and specs C01, C02, C08, C09, C11, C12, C16 and F06.
- **Implementation:** not built. The spec plans no new runtime dependency. The parser is
  written by hand from the openCypher grammar. The TCK's feature files (Apache-2.0)
  would be test data, read from a pinned checkout and never linked into a binary.
- **Rejected** (spec §15):
  - Gremlin as the property-graph language;
  - Cypher writes in this spec;
  - a stored property-graph copy of the data;
  - a separate Cypher executor;
  - generating SPARQL text instead of algebra;
  - one row per value, or an arbitrary value, for multi-valued properties;
  - `rdf:type` triples as relationships;
  - unasserted reifications as relationships;
  - case-insensitive or case-converted names;
  - Bolt in the first phase;
  - a parser generated from the ANTLR grammar.

## Encryption at rest and encrypted backups

- **Spec:** [`F11-encryption-at-rest.md`](F11-encryption-at-rest.md), written on
  2026-10-02 independently from:
  - the Sparkles code, the file layout of a database built with the release binary, and
    the [F05](F05-snapshot-repositories.md), [C13](C13-automatic-compaction.md) and
    [X01](X01-compression-codecs.md) specs;
  - NIST SP 800-38D, draft-irtf-cfrg-xchacha-03 and the FastCDC paper (USENIX ATC 2016);
  - the public documentation of restic and borg (BSD-licensed), CockroachDB, MongoDB,
    SQLCipher, AWS KMS, Google Cloud KMS, HashiCorp Vault Transit, OpenZFS and Linux
    fscrypt, the PostgreSQL wiki page on transparent data encryption, and RocksDB's
    public `env_encryption.h` header, all read for design ideas only. No source code of
    these projects was read;
  - RFCs 2104, 5869, 8439, 8452 and 9106, NIST SP 800-108, the age format and the PADMÉ
    padding paper, cited from general knowledge.
- **Planned dependencies:** none new for Phases 1 and 3 beyond moving existing crates.
  `aws-lc-rs` 1.18 (ISC AND Apache-2.0-or-ISC) is already in the tree through `rustls`.
  It becomes a direct dependency of `sparkles-backup`, and of `sparkles` behind a `crypt`
  feature, for AES-256-GCM, HKDF and HMAC. `argon2` 0.6 (MIT/Apache-2.0) and `zeroize` 1.x
  (MIT/Apache-2.0) are already dependencies. Phase 2 adds the `age` crate
  (MIT/Apache-2.0). The AWS and Google Cloud KMS clients would come behind cargo features
  (spec open question 11). FastCDC is to be written from the paper, because it needs a
  keyed gear table.
- **Rejected** (spec §13):
  - unauthenticated CTR or XTS modes at the file layer;
  - decrypting whole indexes into memory at open;
  - SQLCipher-style fixed pages for the permutation files;
  - random 96-bit GCM nonces, XChaCha20-Poly1305 as the only cipher, and AES-GCM-SIV;
  - convergent encryption for backups;
  - backing up sealed files instead of their logical plaintext;
  - encrypting only the vocabulary;
  - a 7-day default rotation of data keys.

## ARQ's query language extensions

- **Spec:** [`G06-arq-query-extensions.md`](G06-arq-query-extensions.md), written on
  2026-10-02 independently from:
  - the Sparkles code and the vendored spargebra, whose `sep-0006` feature already
    parsed `LATERAL`;
  - Apache Jena's `jena-arq` sources (Apache-2.0): the ARQ grammar `Grammar/main.jj`,
    `SyntaxVarScope`, `OpLateral` and its executor, the `path` package (`P_Mod`,
    `P_FixedLength`, `PathFactory`, `PathCompiler`, `PathLib`, `PathEngineSPARQL`,
    `PathEngineN`), `Template` and `TemplateLib`, and Fuseki's `SPARQLQueryProcessor`
    and `Responses`, read for syntax and behaviour. No code was copied;
  - ARQ's test suites `testing/ARQ/Syntax-Lateral`, `testing/ARQ/Lateral` and the
    `syntax-quad-construct-*` tests of `testing/ARQ/Syntax-ARQ`, and the unit tests
    `TestPath` and `TestPathQuery`, whose cases are ported with citations to
    `crates/sparkles/tests/arq_syntax.rs`;
  - the output of Jena 6.2.0's `arq` command on small graphs;
  - SPARQL 1.1 and 1.2 Query and the SPARQL 1.2 community proposal SEP-0006.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. The `LATERAL`
  decorrelation reuses the analysis of the EXISTS decorrelation, and its per-row
  evaluation the planner's substitution of constants. **Dependencies:** none new.
- **Rejected** (spec §9):
  - expanding ranges in the parser;
  - evaluating every `LATERAL` per row, or decorrelating every one into a join;
  - renaming hidden sub-select variables in the algebra;
  - copying Jena's evaluation of `{0,}` as `{+}`;
  - ARQ's syntax off by default;
  - returning quads from `Dataset::construct`.
- **Phase 2 spec** (§11, configurable DESCRIBE), written on 2026-10-02 independently
  from:
  - the Sparkles code, whose DESCRIBE followed the blank-node closure in the default
    graph;
  - Apache Jena's `jena-arq` sources (Apache-2.0): the `core.describe` package
    (`DescribeHandler`, `DescribeHandlerRegistry`, `DescribeBNodeClosure`), `util.Closure`
    and `QueryExecDataset.describe`, and TDB2's `QueryEngineTDB`, read for behaviour. No
    code was copied;
  - the output of Jena 6.2.0's `arq` command on a small TriG dataset;
  - the W3C member submission *CBD - Concise Bounded Description* (2005), for the
    concise bounded description and its symmetric form, and RDF 1.2 Concepts for
    reifiers.
- **Phase 2 implementation** (2026-10-02): from the spec and the Sparkles code.
  **Dependencies:** none new.
- **Rejected in Phase 2** (spec §11.8):
  - a registry of handlers;
  - reifiers off by default;
  - failing a query at `maxTriples`;
  - the union graph as the default source in a union store.

## OpenAPI description, shell completions and man pages

- **Spec:** [`X03-openapi-and-completions.md`](X03-openapi-and-completions.md), written on
  2026-10-02 independently from:
  - the OpenAPI Specification 3.1.1 and JSON Schema 2020-12;
  - YAML 1.2.2, for the emitter's scalar and block rules;
  - the SPARQL 1.1 Protocol, the Graph Store HTTP Protocol, RFC 9110, RFC 6265,
    RFC 7617, RFC 6750 and RFC 9512;
  - the docs.rs documentation of `clap_complete` and `clap_mangen`, and the nixpkgs
    manual on `installShellFiles`;
  - the Sparkles code and `docs/API.md`.
- **Implementation** (2026-10-02): from the spec and the Sparkles code. The document is
  built by Sparkles' own code, and the YAML emitter is its own. `utoipa` and `schemars`
  were considered and not used (spec §2.1). **Dependencies:**
  - `clap_complete` 4.6.11 (MIT OR Apache-2.0), for `sparkles completions`;
  - `clap_mangen` 0.2.33 and `roff` 1.1.1 (MIT OR Apache-2.0), for `sparkles man`.

  All three are linked into the `sparkles` binary.
- **Checked with** (not linked or shipped): Redocly CLI (MIT), `openapi-typescript`
  (MIT) and `yq` (MIT), each run once by hand at landing.
- **Rejected** (spec §1 and §2.1):
  - per-handler `utoipa` attributes and `schemars` derives;
  - a hand-written YAML file as the source;
  - a bundled Swagger UI or Redoc;
  - Nushell completions, which need another crate.

## Development tools (not linked into Sparkles)

- **Adopted** (2026-09-30, with the pre-commit hooks in `.pre-commit-config.yaml`):
  - `prek` 0.5.4 (MIT), the hook runner, pinned in `mise.toml`;
  - `oxfmt` 0.71.0 (MIT), the UI formatter, a devDependency in `ui/package.json`
    (`ui/pnpm-lock.yaml`), with its `@oxfmt/binding-*` 0.71.0 platform packages (MIT) and
    `tinypool` 2.2.0 (MIT);
  - `shfmt` 3.14.1 (BSD-3-Clause, mvdan/sh), pinned in `mise.toml`;
  - `shellcheck` 0.11.0 (GPL-3.0), pinned in `mise.toml`. It is a development tool only.
    It runs on the shell scripts and is never linked, bundled or shipped;
  - `nixfmt` (MPL-2.0), the flake's formatter, pinned by `flake.lock`.
- **Design source:** the tools' own documentation.
- **Replaced:** Prettier and `prettier-plugin-svelte` (MIT), removed from the UI.
