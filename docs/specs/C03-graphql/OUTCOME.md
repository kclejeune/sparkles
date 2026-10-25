# C03: GraphQL read adapter — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part (Phase 1)
>
> **Phases:** Phase 1 shipped on 2026-10-02: the adapter, the schema configuration,
> schema drafts, `/{ds}/graphql` and the CLI. Of Phase 2, the MCP tool `graphql_query`
> came on 2026-10-03 with [C11](../C11-mcp-server.md#outcome), and the UI and stored
> GraphQL queries are not built. Phase 3 is subscriptions, built only if a workload asks
> for them.
>
> **User docs:** [API: GraphQL](../../API.md#graphql) · [Usage: GraphQL](../../USAGE.md#graphql) ·
> [Features](../../server/README.md#server-and-protocols)
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) section at
> the end records how it landed.

## Outcome

**Phase 1 delivered on 2026-10-02.** Of Phase 2, only the MCP tool `graphql_query` is
built, as the later additions below describe. Phase 3 is not built.

- The crate `sparkles-graphql` holds the adapter. `mapping` parses the SDL with
  `apollo-compiler` next to a prelude that declares the four directives, the scalars the
  SDL does not declare, `LangString`, `RDFTerm` and `Node`, and checks it as §3.3 says.
  Every error names its line. `api` writes the API schema as SDL, and `apollo-compiler`
  validates it. `plan` walks the selection tree with the specification's CollectFields
  and makes the fetch groups. `algebra` builds each group as `spargebra` algebra. `exec`
  runs the groups in order on one snapshot and assembles the response. `config` keeps
  `graphql.json` with 20 versions, and `draft` reads SHACL shapes or C02's drafted shapes
  and writes the commented SDL.
- The engine gained `QueryOptions::work`, the row count of `--max-rows-produced`, shared
  by every group of a request. The groups also share one deadline, and the serialized
  response counts against `--max-result-mb`.
- The server's `http::graphql` module serves `/{ds}/graphql`, `/{ds}/graphql/schema`,
  `/$/graphql/{ds}`, `…/versions` and `…/draft`, behind the default-on `graphql` feature.
  Grants gained the endpoint `graphql`, which `query` covers. Requests count against the
  `query` rate-limit class, are measured as `operation="graphql"`, fill the histogram
  `sparkles_graphql_groups`, and log the operation name, the document hash and the
  number of groups. `serve` has `--graphql-max-depth`, `--graphql-max-nodes`,
  `--graphql-default-first` and `--graphql-max-first`. `sparkles graphql --loc DB` has
  `run`, and `schema get|put|delete|versions|draft`. Backups and clones of a persistent
  dataset carry `graphql.json`.

**Deviations and additions.**

- The response is assembled by `apollo-compiler`'s `resolvers::Execution`, whose
  resolvers read the groups' rows and never query. It gives the specification's null
  propagation, `__typename` and introspection. A field error carries its code through a
  table the resolvers fill, since the executor's errors have a message only.
- `VALUES` cannot hold blank nodes, so each blank node parent of a child group is one
  `UNION` branch whose variable is bound through `QueryOptions::initial_bindings`. The
  explained text of such a group names a variable bound outside it, so A17 holds for
  groups whose parents are IRIs.
- An order key sorts a node by its smallest value, or its largest one for `_DESC`, with
  `GROUP BY` and `MIN` or `MAX`. A node with two values of an order field then appears
  once instead of twice.
- String filters compare the lexical form of literals, `STR(?x)`, so `eq: "Ann"` matches
  `"Ann"@en` as well. The other scalars compare typed values.
- `TFilter` has no `exists` for object fields. `not: { worksFor: {} }` says that a node
  has no value, since an empty filter on an object field holds when some value exists.
- The default language range `"*"` matches every value, tagged or not, so a `String`
  field over plain literals needs no `@lang`. A `lang` argument that names other ranges
  filters as §4.5 says.
- Drafts type a field whose class has no drafted type, and a field with `sh:nodeKind
  sh:IRI`, as `Node` rather than `Resource`. Such a value is answered as its mapped type
  when it has one, and as `Resource` otherwise. Mapping schemas accept fields typed
  `Node` for the same reason.
- Without `source`, a draft reads shapes when a shapes graph is named or a SHACL guard is
  installed, and the data otherwise. A non-null field is backed when the guard runs in
  `reject` mode with a `strict` baseline over exactly the schema's `dataGraph`.
- A5's document, `allPerson(first: 1000) { nodes { name knows { name } } }`, estimates
  1,000 + 1,000 × 100 = 101,000 nodes with the default `defaultFirst`, one more than
  `maxNodes` allows. The tests run it with `knows(first: 10)` or a raised limit.
- `persistedOnly: true` is refused at `PUT`, because stored GraphQL queries are Phase 2.
  The CLI's `schema put` has no `--force` flag, because no stored query can break. The
  flag arrives with stored queries.
- A cursor's `h` hashes the field's name, not its alias, with its filter and order and
  the schema version.
- A `PUT` with `application/graphql` replaces the SDL and keeps the other fields of the
  installed configuration.
- A malformed HTTP request has the code `BAD_REQUEST`, and an operation name that the
  document does not have is `GRAPHQL_VALIDATION_FAILED`. A `403` from the access layer
  has Sparkles' JSON error body, not a GraphQL response.
- `explain=true` also returns `extensions.sparkles.timing`, the time of parsing,
  planning, the groups and assembly, and `groups`, the number of engine executions.
- The configuration of an in-memory dataset is not in its backups, as for stored queries.

**Tests.** The crate's tests cover A4 to A11, A14, A15 and A17, abstract types, blank
nodes, mapping errors with their lines and the non-null warnings. A differential test
runs 400 random documents over 40 random graphs, with nested `and`, `or`, `not` and
object filters, orders, slices, nested lists and multi-valued fields, and compares each
answer with hand-written SPARQL. The server's tests cover A1 to A3, A6 with
`CURSOR_EXPIRED` after a compaction, A11, A12, A14, A15 and A17 over HTTP, the status
codes of both media types and the metrics. The access-control tests cover A13: a view of
some graphs answers as a store holding only those graphs, a hidden node is null like a
missing one, a `graphql`-only grant reads `/{ds}/graphql` and gets `403` from
`/{ds}/sparql`, a `query` grant covers `graphql`, and protections of triples apply to
lookups, collections and `totalCount`. A CLI test drafts, installs and runs.

**Measurements.** The release build ran on the 1.05M-triple data of
`scripts/gen-data.py` in memory, with mimalloc preloaded and pinned to cores 0–11, with
`crates/sparkles-graphql/tests/bench.rs`. The machine was shared with other builds, with
a load average of 45 to 49 on 16 cores, so absolute times are inflated and vary by up to
2× between runs. Each value is the median of 21 runs that alternate the four
measurements, with the result cache off.

| Document | Groups | GraphQL ms | Groups as SPARQL ms | Adapter ms | One SPARQL query ms |
|---|---|---|---|---|---|
| `person(id:) { name age knows { name } }` | 2 | 2.58 | 2.35 | 0.09 | 10.98 |
| `allPerson(first: 100) { nodes { id name age } }` | 1 | 26.40 | 25.13 | 0.25 | 24.09 |
| `allPerson(first: 1000) { nodes { name knows(first: 10) { name } } }` | 2 | 56.39 | 39.31 | 7.10 | 47.35 |
| the same page with `filter`, `orderBy`, `totalCount` and `worksFor` | 3 | 23.13 | 21.67 | 0.32 | 52.75 |
| a page of 1,000 with two sibling lists, `knows` and `authorOf` | 3 | 49.79 | 33.29 | 8.82 | 40.83 |

"Groups as SPARQL" runs the groups' explained text through `sparkles::sparql::query`.
"Adapter" is parsing, planning and assembly. "One SPARQL query" is the query a developer
would write with nested `OPTIONAL`s, without per-parent limits. For lookups, pages and
filtered pages, the adapter adds 0.1 to 0.3 ms and stays within §15's target. For a
response of about 3,600 nodes, assembly takes 7 to 9 ms, about 2 µs per node, and the
request is about 40% slower than its groups run as SPARQL, which misses the 10% target.
Assembly runs the generic executor once per object and builds a JSON value per field,
and it was not profiled further. The single SPARQL query was slower than the adapter for
the lookup and the filtered page, and faster for the two pages of 1,000 people.

**Not built.** Phase 2 holds the UI's GraphQL page and schema editor, stored GraphQL
queries, `persistedOnly`, the MCP tools of stored queries, `QueryOptions::seed` and the
per-group top-k, and none of these is built. Its `graphql_query` tool came later. Phase 3
holds subscriptions and is not built. Nested
lists are not connections, and a nested `first` limits the response while the engine
produces every value of the field.

**Later additions (2026-10-03).** The MCP server gained `graphql_query`, which runs a
document on a dataset with a schema installed, refuses mutations, and returns the API
schema when called without a document ([C11](../C11-mcp-server.md#outcome)). It answers
with the GraphQL response as JSON, not compacted as tables are, and there is no separate
`graphql_schema` tool. Stored GraphQL queries and their tools are still not built.

### Roadmap follow-ups (2026-10-12)

**Execution: seeded child groups and per-parent top-k** (item 37, Tier 4,
[ROADMAP](../../ROADMAP.md)). Both are Phase 2 items of §16 and *Not built* above.

* **What happens today.** `run_groups` in `crates/sparkles-graphql/src/exec.rs` passes a
  child group the distinct node terms of its parent group (`node_ids`), and
  `algebra::seed` turns them into a `VALUES` block of IRIs that the engine looks up in
  its dictionary again. Each blank-node parent becomes its own `UNION` branch with a
  bound variable, so a page of 1,000 blank-node parents plans a union of 1,000
  branches. The child query has no limit per parent: `slice` (`exec.rs`, applied to
  `FieldPlan::Multi` and list `FieldPlan::Object` fields in `resolve_field`) cuts each
  parent's values to `offset` and `first` after the engine produced all of them.
* **`QueryOptions::seed`** (§6.2). The parent group's result keeps its node column as
  engine ids (the `QueryResult` table already holds ids, decoded only for assembly).
  The child query declares the seed variable, and `QueryOptions::seed` binds it to that
  id column before planning, as a leaf with a known size that the join planner orders
  first. Ids are only valid within one snapshot, which every group of a request
  shares, so no check beyond the snapshot identity is needed. Blank nodes need no
  special case. `explain` renders the seed as `VALUES ?p { … }` with the decoded terms,
  so the text stays runnable SPARQL.
* **Per-parent top-k.** A `TopKPerGroup { key, order, offset, limit }` plan node keeps,
  for each value of `key`, the rows ranked `offset` to `offset + limit` by `order`,
  with a bounded heap per key, or by a streaming cut when its input is already sorted
  by `(key, order)`, as a child group's PSO scan seeded by parent ids is. The
  adapter emits it for nested `first` and `offset`, and `slice` becomes a no-op check.
  The node estimate of §7.1 then counts the values produced, not only those returned.
  The operator is not exposed in SPARQL syntax; F13 and U03 may reuse it.
* **Acceptance.** The benchmark row `allPerson(first: 1000) { nodes { name knows(first:
  10) { name } } }` produces at most 10,000 `knows` rows (the plan counter of the
  child group says so) and its time drops toward the "groups as SPARQL" column, and
  every document of the random differential test returns the same response.

**Nested connections.** Nested lists are plain lists (*Not built* above). A list field
whose target type is a node type gains a connection form, `knowsConnection(first:,
after:, last:, before:, filter:, orderBy:)` with `edges { cursor node }`, `pageInfo`
and `totalCount`, generated beside the list field. The cursor is the parent's id with
the item's position, bound to the request's commit as top-level cursors are. Per-parent
windows are the top-k operator above; `totalCount` per parent is a grouped count over
the same seeded query.

**Persisted queries, `persistedOnly` and their MCP tools.** §11 designs stored GraphQL
queries, `persistedOnly` and A16; `PUT` refuses `persistedOnly: true` today. They are
built on [C16](../C16-stored-queries.md)'s stored queries, with a `language: "graphql"`
member, so versions, runs by name, parameters (GraphQL variables typed by the schema)
and the MCP tools of stored queries come from C16 and C11 unchanged. A document is
validated against the installed schema when stored and again when the schema changes;
a document the new schema breaks is marked `invalid` and refuses to run.

**Mutations.** §12 keeps them out of scope. A design would generate, per node type,
`create<T>`, `update<T>` and `delete<T>` from the same `@rdf` mapping, each compiled to
one SPARQL update under the caller's write grant and protections, run as one commit
per request through the guard of C10 and the previews of C15 (`dryRun: true`), with the
commit receipt in the response's `extensions`. It waits for a user who needs writes
through GraphQL rather than SPARQL or the Graph Store.

**Subscriptions.** §13's design re-runs a document after each commit that touches its
read predicates. [F13](../F13-incremental-views-and-provenance.md)'s live queries give the
incremental form: a subscription compiles its groups to F13 views and sends the
changed nodes. It is built on F13 rather than as a re-run loop.

### Completion contract

Roadmap item 37 covers the remaining Phase 2 UI and schema editor as well as seeded
execution, per-parent windows, nested connections and persisted queries with their
MCP controls. Verify schema edits and invalidation, stored-only refusal, caller
visibility and the existing differential query tests. The top-k acceptance counter
must bound rows produced per parent, rather than only slicing the assembled JSON.

Subscriptions remain conditional on a workload adopting Phase 3 and use F13 under
item 17. Mutations remain a separately approved design, as §12 and the follow-up
above state. Neither conditional extension can be treated as delivered merely because
Phase 2 completes, and declining one must be recorded explicitly.
