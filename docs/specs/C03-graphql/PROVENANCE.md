# GraphQL read adapter

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`C03-graphql.md`](README.md), written on 2026-10-02 independently
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
