# Provenance of adopted designs and dependencies

The clean-room provenance record of Sparkles. One entry per feature or dependency: where
the design came from, what was adopted (version and license), and what was rejected. No
entry uses Fluree source, tests or design documents: Fluree was not consulted for any
feature. Each feature's spec in this directory (see the [index](README.md)) gives the
full design and, in its Outcome section, how the implementation landed.

## Allocator (server and CLI binaries)

- **Adopted:** `mimalloc` 0.1.52 (MIT), with `libmimalloc-sys` 0.1.49 (MIT, `extended`
  feature for `mi_collect`), which bundles Microsoft mimalloc 3.3.2 (MIT). It is a
  default-on feature of `sparkles-server`; the `sparkles` library stays
  allocator-neutral.
- **Design source:** our own measurements of allocator memory retention, the mimalloc
  option documentation and source
  (`src/options.c`: `purge_delay`, `purge_decommits`), and glibc `mallopt(3)` and
  `malloc_trim(3)`.
- **Rejected:**
  - glibc `mallopt` fixed thresholds: slower throughput.
  - `tikv-jemallocator` 0.7 (MIT/Apache-2.0): slower, and needs `make`.
  - mimalloc `PURGE_DELAY=0`: slower throughput.

## Durable commit identity

- **Spec:** [`CI-commit-identity.md`](CI-commit-identity.md), written independently from
  the Sparkles code, the Apache Jena Fuseki sources (Apache-2.0), the W3C SPARQL 1.1
  Protocol and Graph Store Protocol, and RFCs 3339, 9562, 9110, 6648 and 9651. Fluree was
  not consulted.
- **Implementation:** from the spec plus Sparkles code only.
  - **Dependencies:** `uuid` 1.x (Apache-2.0/MIT, already a dependency; its `serde`
    feature is now enabled) and the CRC-32 from `flate2` (MIT/Apache-2.0, already a
    dependency).
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
  from the Sparkles code, Apache Jena's text-query documentation and `jena-text` sources
  (Apache-2.0, syntax compatibility only), the Tantivy docs, spargebra's parser source,
  the SPARQL 1.1 Query §4.2.3 collections syntax, BCP 47 and RFC 4647. Fluree was not
  consulted.
- **Adopted:** `tantivy` 0.26.2 (MIT) with default features off, plus `mmap`, `stemmer`
  and `lz4-compression`, which avoids zstd's C code. It is behind the optional `text`
  feature of `sparkles`, which the server enables by default.
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

## Vector similarity (exact search)

- **Spec:** [`F04-vector-search.md`](F04-vector-search.md), written independently from
  RDF 1.2 Concepts, SPARQL 1.1 §17.6, RFC 8259, IEEE 754, RFC 8141, the HNSW paper,
  docs.rs / crates.io pages for the ANN candidates, and the Sparkles code. Fluree was not
  consulted.
- **Implementation:** Phase 1 (exact search), from the spec and Sparkles code only.
  No new dependencies: the kernel is our own 8-lane loop, and rayon is already used.
- **Deferred:** HNSW (USearch 2.26 Apache-2.0 recommended by the spec, hnsw_rs as the
  pure-Rust alternative).
- **Rejected** (spec §8): canonicalizing vector literals on load; a new id tag for
  vectors; `rdf:JSON`.

## Named snapshots and point-in-time reads

- **Spec:** [`F06-snapshots-and-point-in-time.md`](F06-snapshots-and-point-in-time.md),
  written on 2026-09-30 independently from the Sparkles code, [CI](CI-commit-identity.md),
  RFC 7089 (Memento), RFC 9110, and the W3C SPARQL 1.1 Protocol and Graph Store Protocol
  (all fetched), plus RFCs 3339, 8246, 6648 and 9651 and Jena's RDF Patch format
  (Apache-2.0, open question only), cited from general knowledge. Fluree was not
  consulted.
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

  Fluree was not consulted.
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
  - **Not adopted:** `openidconnect` 4.0.1 (MIT) and `cookie`, which the spec named. The
    OIDC relying party (discovery, code flow with PKCE, claim checks), HMAC-SHA-256 and
    cookie handling are our own code over `jsonwebtoken`, `reqwest` and `sha2`.
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

## Write-time SHACL validation

- **Spec:** [`C10-write-time-validation.md`](C10-write-time-validation.md), written on
  2026-09-30 independently from the Sparkles code and specs (CI, C08, C06, C01), W3C
  SHACL, SHACL 1.2 Core (Working Draft), SPARQL 1.1 Update and Protocol, and Apache Jena's
  SHACL documentation (Apache-2.0), all fetched; plus RFC 9110, RFC 9651 and four papers
  on integrity checking and view maintenance (Nicolas 1982; Blakeley, Larson and Tompa
  1986; Gupta and Mumick 1995; Corman, Reutter and Savković 2018), cited from general
  knowledge. Fluree and TopBraid documentation were not consulted.
- **Implementation:** from the spec plus Sparkles code only (2026-09-30). No new
  third-party dependencies: `sparkles-shacl` now also uses `serde`, `serde_json`,
  `parking_lot`, `sha2` and `tempfile`, all already in the workspace.
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
  independently from the MCP specification, revision `2026-07-28` (fetched from
  modelcontextprotocol.io: versioning, changelog, base protocol, stdio and streamable HTTP
  transports, tools, resources, prompts, pagination, caching, cancellation, authorization
  and the schema), the rmcp 3.5.0 crates.io record, docs.rs pages, READMEs and `LICENSE`,
  JSON-RPC 2.0 and SPARQL 1.1 (cited from working knowledge), and the Sparkles code and
  specs C01, C02 and C06. Fluree was not consulted, including any Fluree MCP server or "memory" feature
  material.
- **Implementation:** from the spec plus Sparkles code only (2026-09-30), behind the
  default-on `mcp` feature of `sparkles-server`.
  - **Dependencies:** `rmcp` 3.5.0 (Apache-2.0), the official Rust SDK
    (github.com/modelcontextprotocol/rust-sdk), pinned `~3.5` with default features off
    and only `server` and `transport-io`, so no tool macros. Its dependencies are MIT,
    Apache-2.0 or both, including `schemars` 1.2.2 (MIT). The published crate carries no
    `LICENSE` file; the text is in the upstream repository.
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
  2026-09-30 independently as internal engineering, from the codec formats' own
  specifications (RFC 8878 zstd, RFC 7932 Brotli, RFC 1952 gzip, the LZ4 frame format),
  RFC 9110 content codings, the docs of `zstd`, `ruzstd`, `brotli`, `flate2`, `lz4_flex`,
  tower-http 0.7, rust-embed 8 and Tantivy 0.26, and the Sparkles code. It is not derived from any other database
  product. It has no explicit "Fluree was not consulted" line, and Fluree is not among
  its sources.
- **Implementation:** Phase 1, from the spec plus Sparkles code only (2026-09-30).
  - **Dependencies:**
    - `zstd` 0.14.0 (BSD-3-Clause), with `zstd-safe` 8.0.0 and `zstd-sys`
      2.1.0+zstd.1.5.7 (BSD-3-Clause; a C build of libzstd 1.5.7) and the `zstdmt`
      feature. It is behind the `zstd` feature of `sparkles`: off in the library, on in
      the server. The spec's table says MIT/Apache-2.0; the crate's `Cargo.toml` says
      BSD-3-Clause (0.13 was MIT).
    - `brotli` 8.0.4 (BSD-3-Clause AND MIT), pure Rust, with `brotli-decompressor` 5.0.3
      (BSD-3-Clause/MIT) and `alloc-no-stdlib` 2.0.4 and `alloc-stdlib` 0.2.4
      (BSD-3-Clause). It is behind the `brotli` feature, likewise off in the library and
      on in the server.
    - tower-http 0.7.1 (MIT) `decompression-full`, which adds `async-compression` 0.4.49
      and `compression-codecs` 0.4.44 (MIT OR Apache-2.0) over the same codec crates.
    - Tantivy's `zstd-compression` feature, which uses `zstd` 0.13.3 (MIT, `zstd-safe`
      7.3.0) over the same `zstd-sys`.
    - `http-body-util` 0.1.5 (MIT), new in the server. `flate2` 1.1.10 and `lz4_flex`
      0.14.0 were already dependencies.
    - The UI's precompression step (`ui/scripts/precompress.mjs`) uses Node's built-in
      zlib only.
  - **Not adopted:** `ruzstd` (MIT) and the `zstd-decode-pure` feature (spec §3).
- **Deferred:** Phase 2 storage codecs (index blocks, vocabulary, spill files), gated on
  measurements; Phase 3 repository blobs with F05.
- **Rejected** (spec §1 non-goals; there is no rejected-alternatives section):
  compressing the WAL, `delta.vocab` or `commits.bin`; in-memory compressed caches;
  compressed network protocols other than HTTP `Content-Encoding`.

## Backup repositories

- **Spec:** [`F05-snapshot-repositories.md`](F05-snapshot-repositories.md), written on
  2026-09-30 independently from:
  - the Sparkles code and specs (CI and F06 in full; C09 §§3–4, 7, 9);
  - Elasticsearch and Kibana public user documentation on snapshot and restore, as
    evidence of desired behaviour only (both are source-available under non-permissive
    licenses; no source code was read);
  - the `object_store` 0.14.2 docs (MIT/Apache-2.0), the AWS S3 User Guide (conditional
    writes, consistency), restic's design references (BSD-2-Clause; ideas only) and the
    `croner` 4.0.0 docs (MIT), all fetched;
  - `chrono-tz`, `lz4_flex`, `age`, BLAKE3, S3 limits, the borg chunker (BSD-3-Clause),
    `s3s-fs`, the MinIO license, RFCs 9110, 3339 and 9562 and the IANA time zone
    database, cited from general knowledge.

  Fluree was not consulted.
- **Implementation:** done (2026-09-30/10-01), from the spec, the implementation plan
  (which overrides the spec where they differ) and the Sparkles code only:
  - the `sparkles-backup` crate (repository engine, policies as pure functions);
  - the engine's capture, lease, `reidentify` and `restoredFrom` support in `sparkles`;
  - the server's `backup` module (registry, task slots, HTTP routes, in-place restore and
    startup recovery, policy scheduler, metrics) behind the default-on `backup` feature
    of `sparkles-server`;
  - the `sparkles repo` / `sparkles backup` commands, the UI's Backups page,
    `docs/API.md` ("Backup repositories") and `scripts/backup-bench.sh`.

  Phase 1 as delivered is spec Phase 1 plus policies, GC and the full UI, as the
  maintainer decided.
  - **Dependencies** (versions from `Cargo.lock`, licenses from each crate's
    `Cargo.toml` and checked against its shipped license files):
    - `object_store` 0.14.2 (`MIT/Apache-2.0`), `default-features = false`, features
      `tokio`, `fs` and `aws` (the crate features `fs` and `s3` of `sparkles-backup`,
      both default); `gcp` and `azure` stay behind the off-by-default `gcs` and `azure`
      features. It reuses the workspace's reqwest 0.13, hyper 1 and aws-lc-rs/rustls.
      Note: the published crate ships only the Apache-2.0 text (`LICENSE.txt`) and the
      ASF `NOTICE.txt`; distributing a binary under the Apache-2.0 option means carrying
      that NOTICE ("Apache Arrow Object Store, Copyright 2020-2026 The Apache Software
      Foundation"). Not a blocker.
    - `croner` 4.0.0 (MIT; `LICENSE.md`), with `derive_builder` 0.20.2 (MIT OR
      Apache-2.0; `darling` 0.20.11, MIT, a build-time proc macro) and `strum` 0.27.2 /
      `strum_macros` 0.27.2 (MIT). Builds with chrono 0.4.45, so the planned fallback
      parser was not needed.
    - `chrono-tz` 0.10.4 (MIT OR Apache-2.0; the bundled IANA tz data is public domain),
      with `phf` / `phf_shared` 0.12.1 (MIT) and `siphasher` 1.0.4 (MIT OR Apache-2.0).
      No `chrono-tz-build` step (the 0.10 tables are pre-generated).
    - New through `object_store`: `crc-fast` 1.10.0 (MIT OR Apache-2.0; pure Rust, no
      build script) with `digest` 0.10.7 and `crypto-common` 0.1.7 (MIT OR Apache-2.0),
      `generic-array` 0.14.7 (MIT) and `spin` 0.10.1 (MIT); `humantime` 2.4.0 (MIT OR
      Apache-2.0); `itertools` 0.15.0 (MIT OR Apache-2.0; a second version next to
      0.14.0); `nix` 0.31.3 (MIT); `quick-xml` 0.41.0 (MIT; next to 0.37.5);
      `wasm-streams` 0.5.0 (MIT OR Apache-2.0; wasm targets only, never built here).
    - Reused, already in the workspace: `sha2`, `lz4_flex`, `futures`, `bytes`,
      `async-trait`, `percent-encoding`, `tokio`, `uuid`, `chrono`, `serde_json`, `toml`.
    - Nothing non-permissive: every new crate is MIT, Apache-2.0 or both. MinIO
      (AGPL-3.0) remains an external test process only (the S3 tests run only with
      `SPARKLES_TEST_S3_ENDPOINT`); `age` (Phase 3 encryption) is not a dependency yet.
  - **Deviations from the spec** (decisions of the plan and of the implementation):
    - Leases are keyed by generation, not by commit (`Lease { generation, label }`;
      history GC keeps a leased generation unconditionally, shown as `backup:<name>`):
      after a compaction the new generation's base covers the captured commit, so a
      lease by commit would not have kept the old generation.
    - `validation.json` and `validation-shapes.ttl` are backed up as meta files (the
      spec's file list omitted them; clones copy them too), and the restore path grammar
      accepts them.
    - Repositories registered through the API are restricted beyond the spec: S3
      credentials only by name (`{"source": "named", "name": …}`, resolved from the
      config file's `[credentials.<name>]` tables), never `env`, `file` or `default`
      chosen by the caller; S3 endpoints (and every address their host resolves to)
      under the server's outbound policy, with connections pinned to the checked
      addresses through a custom DNS resolver; `fs` paths under `[api] fs_roots` when
      set and outside the directories of the server's config files; `gcs`/`azure`
      from the config file only. Endpoint URLs refuse userinfo, queries and fragments.
    - Blob header byte 5 is the codec (0 raw, 1 LZ4, 2 zstd reserved) and byte 6 the
      encryption (0 none); still format 1.
    - In-place restores answer requests for the dataset `503 dataset-restoring` with
      `Retry-After: 5` from a router layer (the spec's check in `dataset()` could not
      guarantee "never 404"); server-wide tasks have `dataset: ""` and are listed for
      `server-admin` only; `verified` comes from a server-local `<data>/backup/verify.json`;
      `Repository.lastGc` from `gc/last.json`; `<data>/sparkles-server.lock` makes one
      server per data directory and guards `sparkles backup restore --data`.
    - Schedules: croner in naive local time with Sparkles' own DST mapping (skipped
      times run after the gap, repeated ones once); `every <duration>` is epoch-aligned
      in UTC.
    - At most 4 tasks wait per `--backup-max-tasks` slot (`503 too-many-tasks` beyond).
    - Not as specified (yet): `507 insufficient-storage` is defined but no restore
      checks free space; `sparkles_backup_object_requests_total` counts only the
      successful requests that verification and GC reports carry (`result="ok"`), not
      every request with `ok|error`; in-memory datasets answer
      `501 backup-unsupported`; `sparkles backup policy run` (offline) is refused
      (Phase 2).
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
  as internal engineering, from:
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

  No code was read or copied from any GPL or LGPL project. It has no explicit "Fluree was
  not consulted" line, and Fluree is not among its sources.
- **Implementation** (slice 1, 2026-09-30: SPARQL queries and updates, `sparkles fmt`,
  `POST /$/format`, the UI's Format button): from the spec, the slice plan and the
  Sparkles code only. Fluree was not consulted.
  - **Design sources:**
    - Philip Wadler, "A prettier printer" (1998/2003), for the document algebra (text,
      line, group, nest) and the fits-then-break rule;
    - Prettier's printer algorithm as its public docs and `commands.md` describe it (MIT):
      `group`, `indent`, `line`/`softline`/`hardline`, `ifBreak` with a group id,
      `lineSuffix`, `breakParent`, `cursorOffset`, `--check`/`--list-different`/`--write`
      and their exit codes, ignore files and `prettier-ignore`;
    - rust-analyzer's syntax-tree approach from its public architecture and design docs
      (MIT OR Apache-2.0): a lossless token stream, a hand-written recursive-descent
      parser with markers (`start`/`complete`/`precede`), trivia kept by token adjacency;
    - the W3C SPARQL 1.1 and 1.2 grammars (EBNF, terminals, keyword spellings), with
      spargebra 0.4.7 (MIT OR Apache-2.0, already a dependency) as the reference parser and
      the source of the algebra the safety check compares.
  - **New dependencies** (all permissive; `Cargo.lock` and `THIRD_PARTY_LICENSES.md`
    regenerated):
    - `unicode-width` 0.2.2 (MIT OR Apache-2.0): display widths in the printer
      (`sparkles-fmt`);
    - `ignore` 0.4.33 (Unlicense OR MIT): directory walks with gitignore rules and
      `.sparklesfmtignore` (server, feature `fmt`);
    - `similar` 2.7.0 (Apache-2.0): `--diff` unified diffs (server, feature `fmt`; the spec
      named 3, the lock had 2);
    - `proptest` 1.11.0 (MIT OR Apache-2.0): property tests, dev only (`sparkles-fmt`);
    - `toml` 1.1.6 (MIT OR Apache-2.0, already an optional dependency for auth and backup):
      `.sparklesfmt.toml`, now also enabled by feature `fmt`;
    - reused: `thiserror` 2.0.21 (MIT OR Apache-2.0), `rayon` and `tempfile` (parallel
      files and atomic `--write`), `sha2` (the input hash in refusal logs).
  - **Test-only switch:** feature `fault-injection` of `sparkles-fmt`, enabled only through
    dev-dependencies (its own tests and `sparkles-server`'s); with
    `SPARKLES_FMT_FAULT=drop-token` the printer drops a token so the tests can show that
    the CLI and the endpoint refuse such output.
- **Later slices** (2026-10-01: Turtle, TriG, N-Triples, N-Quads and JSON-LD,
  `sparkles lsp`, the optional WebAssembly build for the UI, streaming Turtle and TriG):
  from the spec, the plan for the remaining slices and the Sparkles code only.
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

## Patched spargebra (vendored)

- **Adopted:** `spargebra` 0.4.7 (MIT OR Apache-2.0, per its `Cargo.toml`; copyright the
  Oxigraph developers) from crates.io, copied to `vendor/spargebra` and used through
  `[patch.crates-io]` in the workspace `Cargo.toml` (2026-09-30).
- **Changes** (`vendor/spargebra/PATCHED.md`), written against 0.4.7's rust-peg grammar
  from the W3C SPARQL 1.1 and 1.2 Query grammars and the W3C test suite, not ported from
  upstream:
  - left-associative `+ -` and `* /`;
  - case-insensitive `true` / `false`;
  - longest match for `<` against `IRIREF`;
  - the FILTER scope in OPTIONAL;
  - SELECT expressions see earlier aliases;
  - no nested aggregates;
  - triple-term subjects restricted per the SPARQL 1.2 grammar rules [123] and [138].
- **Notes:** like the published crate, the copy has no `LICENSE` files; the license is
  the `Cargo.toml` field. The copy is dropped once a spargebra release has these fixes.
- **Rejected:** waiting for an upstream release, and porting the rewritten parser of
  Oxigraph's development version.

## GeoSPARQL

- **Spec:** [`G01-geosparql.md`](G01-geosparql.md), from the OGC GeoSPARQL 1.1 standard
  (OGC document license), Apache Jena's `jena-geosparql` (Apache-2.0; read for behaviour,
  the `spatial:` and `spatialF:` surface and its divergences, no code copied), Oxigraph's
  `spargeo` (Apache-2.0/MIT, read), QLever's spatial support (Apache-2.0, read) and the
  Sparkles code. Fluree was not consulted.
- **Implementation** (Phase 1, 2026-10-01): the `geo` feature of `sparkles` (on in the
  server), built on `geo` 0.33.1 (default features off, renamed `georust`), `geo-index`
  0.4.0 (both MIT OR Apache-2.0; `wkt` and `geojson` were tried and dropped) and
  `geographiclib-rs` 0.2.7 (MIT; a port of Karney's GeographicLib). New transitive crates (`i_overlay`, `robust`, `rstar`, `geo-traits`,
  `geo-types`, `float_next_after`, `heapless`, `accurate`, …) are all MIT and/or
  Apache-2.0, with no `earcutr` or `spade`; `THIRD_PARTY_LICENSES.md` lists them.
  - The WKT and GeoJSON readers and writers are hand-written from the GeoSPARQL 1.1 and
    Simple Features grammars and RFC 7946 (the `wkt` crate gives no error offsets and has no
    TRIANGLE/TIN). The CRS and unit tables come from the OGC, EPSG and QUDT registries.
  - Topology through DE-9IM (`geo`'s relate); geodesic distance, area and buffers through
    GeographicLib (Karney 2013); GeographicLib's published values are test oracles.
  - The spatial index (packed R-tree base, overlay and tail) and the planner/executor are
    written from the spec and the Sparkles store and planner code.
  - **Test data and tools:** the GPL-2.0 GeoSPARQL Compliance Benchmark is never vendored:
    `scripts/geosparql-benchmark.sh` fetches it at test time into `target/` (pinned commit
    879e0746) only with `SPARKLES_ALLOW_GPL_BENCHMARK=1`; its README and file names (not its
    code) were consulted to write the runner.
- **Phase 2** (2026-10-01): spatial joins, nearest neighbours, query rewrite, hulls and
  aggregates, conversion and the UI's maps, from the spec, the Phase 2 plan and the
  Sparkles code. Fluree was not consulted.
  - **UI dependencies:** `maplibre-gl` 6.11.2 (BSD-3-Clause) and its `@maplibre/*` packages
    (ISC, MIT, MIT OR Apache-2.0), listed in `THIRD_PARTY_LICENSES-UI.md`.
  - **Data:** the bundled basemap is Natural Earth 1:110m, release 5.1.2 (public domain),
    reduced by `scripts/basemap.mjs` (`ui/src/lib/basemap/README.md`).
  - The GeoSPARQL vocabulary axioms behind `--vocab geosparql` are written by hand from the
    standard; no OGC files are vendored.

## ShEx validation

- **Spec:** [`G02-shex.md`](G02-shex.md), written on 2026-09-30 independently from the Sparkles
  code and spec C10; the ShEx 2.1 Final Community Group Report and the ShapeMap draft
  (shex.io; W3C Software and Document License; the shape-map repository is MIT), both
  fetched; the shexTest suite (W3C Software and Document License), upstream and as
  vendored in Apache Jena; Apache Jena's `jena-shex`, `jena-cmds` and `jena-fuseki2`
  sources (Apache-2.0), read for behaviour and CLI surface, no code copied; rudof 0.3.24
  (MIT OR Apache-2.0), inspected for licensing, dependency weight, API and conformance,
  not used as a dependency; and papers by Staworko et al. (ICDT 2015), Boneva, Labra Gayo
  and Prud'hommeaux (ISWC 2017) and Labra Gayo et al. (2015, RBE derivatives). Fluree
  was not consulted.
- **Implementation** (Phase 1, 2026-10-01): from the spec, the plan and the Sparkles
  code. Fluree was not consulted.
  - The conformance harness (`crates/sparkles-shex/tests/shextest.rs`) reads the shexTest
    manifests itself (oxttl for Turtle, serde_json for JSON-LD); Jena's
    `ShexTests.java` and `ShexValidationTest.java` (Apache-2.0) were read for how the
    suite is run (base IRIs, the start-focus and map forms, extension results, its
    exclusions), no code copied.
  - The server endpoint (`http/shex.rs`) and `sparkles shex` (`shex_cmd.rs`) follow the
    `/{ds}/shacl` handler and `sparkles shacl` in the Sparkles code.
  - The ShExC lexer and parser (`shexc/`) are hand-written from the ShEx 2.1 grammar;
    the suite's ShExJ files were the reference for the parsed form. No parser generator.
  - The ShExJ reader (`shexj.rs`) follows the ShExJ section of the ShEx 2.1 report.
  - Schema checks and compilation follow the report's well-formedness and stratification
    rules (Boneva et al. 2017 for stratified negation).
  - The matcher decides a neighbourhood partition by iteration-count intervals per group
    rather than RBE derivatives; the brute-force partition enumerator in its tests reads
    the report's partition definition literally. Typing is a per-stratum greatest fixed
    point (Staworko et al. 2015; Boneva et al. 2017).
  - Node constraints reuse `sparkles::xsd` for lexical forms and value comparison; facet
    semantics follow the report and its errata.
  - Shape maps follow the ShapeMap draft; Jena's compact-map extensions (BASE/PREFIX,
    optional commas, trailing `.`) were taken from reading `jena-shex`, no code copied.
  - Imports and EXTERNAL resolution follow the report; http(s) imports go through
    `sparkles::outbound`.
  - **Dependencies:** none new; `sparkles-shex` uses workspace crates only (`smallvec` and
    `oxilangtag` were already in the lock).
  - **Test data:** shexTest is read from the Jena checkout or `SPARKLES_SHEX_TESTS`,
    not vendored.
  - **Development-only:** the rudof CLI as a differential oracle (`bench:shex
    --compare-rudof`), never linked.
- **Rejected** (spec §12):
  - depending on rudof, or vendoring its parser crate;
  - porting Jena's partition-enumeration validator;
  - goal-directed recursive validation with hypothesis stacks;
  - translating ShEx to SHACL or SPARQL;
  - RBE derivatives as the matcher (the spec preferred them; the interval method was
    adopted in implementation because it is exact and linear, checked by an oracle);
  - ShEx 2.2 in Phase 1;
  - executing semantic actions other than the Test extension;
  - an RDF report format;
  - ShExC literals in named graphs;
  - SHACL and ShEx guards together in Phase 2;
  - a `peg` grammar;
  - treating budget overruns as nonconformant.

## Development tools (not linked into Sparkles)

- **Adopted** (2026-09-30, with the pre-commit hooks in `.pre-commit-config.yaml`):
  - `prek` 0.5.4 (MIT), the hook runner, pinned in `mise.toml`;
  - `oxfmt` 0.71.0 (MIT), the UI formatter, a devDependency in `ui/package.json`
    (`ui/pnpm-lock.yaml`), with its `@oxfmt/binding-*` 0.71.0 platform packages (MIT) and
    `tinypool` 2.2.0 (MIT);
  - `shfmt` 3.14.1 (BSD-3-Clause, mvdan/sh), pinned in `mise.toml`;
  - `shellcheck` 0.11.0 (GPL-3.0), pinned in `mise.toml`. It is a development tool only:
    it is run on the shell scripts and is never linked, bundled or shipped.
  - `nixfmt` (MPL-2.0), the flake's formatter, pinned by `flake.lock`.
- **Design source:** the tools' own documentation.
- **Replaced:** Prettier and `prettier-plugin-svelte` (MIT), removed from the UI.
