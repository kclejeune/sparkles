# C12: Graph-level access control and endpoint permissions

> **Status:** implemented
>
> **Phases:** Phase 1 shipped on 2026-10-02: graph grants for reads and writes, endpoint
> permissions, every read and write path of the server and the MCP server, and the
> engine's filtered dataset view.
> Phase 2, protections of triples by predicate, class and pattern, is specified and
> shipped in [C12b](C12b-triple-access-control.md).
>
> **User docs:** [API: Graph-level access control](../API.md#graph-level-access-control) ·
> [Usage: Restricting users to some graphs](../USAGE.md#restricting-users-to-some-graphs) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec is Phase 3 of [C09](C09-dataset-access-control.md). C09 gave every request a
principal and every principal a level (`read`, `write` or `admin`) on each dataset. A
level covers the whole dataset. This spec lets an operator narrow a grant to some named
graphs, and to some endpoints of a dataset. It was written clean-room from Apache Jena's
documentation of Fuseki's data access control, Solid's access modes, plain role-based
access control and the Sparkles code.

## 1. Summary

**Before this feature**, a principal with `read` on a dataset reads every graph of it,
through every endpoint, and a principal with `write` writes every graph. A team that
keeps several tenants' data in named graphs of one dataset has to split it into datasets,
which loses cross-tenant queries for the principals who are allowed them.

**This spec adds** two optional restrictions to a grant:

- `graphs` lists the graphs the grant covers, by IRI, by IRI pattern with `*`, or by
  Jena's name for the default graph. A principal sees a dataset through a filtered view
  that contains only the graphs its grants cover. Writes may touch only the graphs of its
  `write` grants.
- `endpoints` lists the services of the dataset the grant applies to, with Fuseki's
  names where they exist (`query`, `update`, `gsp-r`, `gsp-rw`, `upload`, `shacl`) and a
  few Sparkles names for the rest.

**Without these restrictions, nothing changes.** Grants without `graphs` and `endpoints`
behave as in C09. A principal whose grants cover every graph runs the same plans, hits the
same caches and gets the same results as before, with the same speed.

### 1.1 Threat model

C09 §1.1 still holds. This spec adds one adversary.

| Adversary | Can | Must not be able to |
|---|---|---|
| A principal restricted to some graphs of a dataset | Query, update and read the graphs its grants cover, through the endpoints they allow. | Read a quad of a hidden graph, learn whether a hidden graph exists or how many quads it holds, change a graph it may not write, or reach an endpoint its grants do not name. |

The guarantee is about answers, not timing or plan shapes. Out of scope:

- timing side channels, as in C09 (a query over a large hidden graph can run slower);
- the plan the planner picks. Estimates come from statistics over every graph, so a
  restricted principal could in principle learn something from the shape of a plan. The
  numbers themselves are removed from the plans such a principal sees (§7.3);
- the ranking of full-text hits. BM25 scores use term statistics of the whole index
  (§5.3);
- the commit sequence. A restricted principal sees that commits happen, because commit
  numbers, entity tags and `Sparkles-Commit` headers are shared by every view (§5.6);
- the configuration itself. An operator who grants a pattern such as `http://ex/*`
  grants every matching graph, including ones created later.

### 1.2 Goals and non-goals

**Goals**

- Restrictions are opt-in, per grant, and monotonic. Grants still form a union with no
  deny rules, so their order never matters.
- The engine enforces the view, not the HTTP handlers. Every scan, path, search and
  statistic of a query reads only the graphs of the view, so a new endpoint built on the
  engine inherits the restriction.
- Every decision is existence-independent, as in C09 §3.3. Whether a request is refused,
  and the shape of its answer, depend on the rules and on the visible graphs only, never
  on whether a hidden graph exists or holds a given quad.
- A principal with full access pays nothing: no extra planning, no different cache
  keys, no different fast paths.
- Fuseki's graph access configuration maps onto these grants line by line (§3.4).

**Non-goals**

- Triple-level or resource-level rules, and rules that depend on data (Solid ACP's
  matchers, attribute-based policies). The unit is a named graph.
- Deny rules.
- Graph restrictions on minted token scopes. A token still narrows only levels; its view
  is its owner's view (§2.4).
- Restricted `admin`. Admin operations (compaction, backups, cloning, reasoning runs,
  index rebuilds, deletion) act on the whole dataset, so `admin` is granted only without
  restrictions.
- Hiding the commit sequence or the size of the dataset on disk from principals who can
  see that the dataset exists.

## 2. Model

### 2.1 Graph grants

A graph grant is a C09 dataset grant with up to two restrictions:

| Field | Meaning |
|---|---|
| `dataset` | A dataset name or `*` pattern, as in C09 §3.3. |
| `level` | `read` or `write`. A restricted grant is never `admin`. |
| `graphs` | Optional. The graphs the grant covers. Absent means every graph. |
| `endpoints` | Optional. The endpoints the grant applies to. Absent means every endpoint. |

Each entry of `graphs` is one of:

- `urn:x-arq:DefaultGraph`, Jena's name for the default graph. The short form `default`
  means the same;
- an absolute IRI, which covers the graph of that name;
- an IRI with `*` wildcards, such as `http://example.org/tenant/a/*`. A `*` matches any
  run of characters, as in dataset patterns. A lone `*` covers every named graph, but not
  the default graph.

Two kinds of graph are never covered by a wildcard:

- **The inferred graph** (`urn:x-sparkles:inferred`). Materialized inferences are derived
  from every graph, so they can restate facts of hidden graphs. A restricted grant covers
  the inferred graph only when it names it exactly.
- **Blank-node graph names.** They have no IRI to match. Only an unrestricted grant
  covers them.

`urn:x-arq:UnionGraph` is not a graph and is rejected in `graphs` (§3.2). The union graph
of a restricted principal is the union of the named graphs it can read (§4).

### 2.2 Endpoints

| Name | Requests |
|---|---|
| `query` | SPARQL queries on `/{ds}/sparql`, `/{ds}/query` and `/{ds}`, plus `/{ds}/explain`, `/{ds}/text`, `/{ds}/geo`, and the MCP tools that query a dataset |
| `update` | SPARQL updates on `/{ds}/update` and `/{ds}`, and the MCP tool `sparql_update` |
| `gsp-r` | Graph Store reads: `GET` and `HEAD` on `/{ds}/data`, `/{ds}/get` and `/{ds}` |
| `gsp-rw` | Graph Store reads and writes on `/{ds}/data` and `/{ds}` |
| `upload` | `/{ds}/upload` |
| `shacl`, `shex` | `/{ds}/shacl`, `/{ds}/shex`, and the MCP validation tools |
| `diff` | `/{ds}/diff` |
| `info` | Every other route that needs `read` or `write` on the dataset: its description, statistics, schema, prefixes, commits, reasoning and index status, snapshots and history settings, validation settings, backups listing and readiness. The MCP tools and resources that describe a dataset count as `info`. |

The first six names are Fuseki's operation names. As in Fuseki, `gsp-rw` includes the
reads of `gsp-r`. Routes that need `admin` have no endpoint name, because only
unrestricted grants give `admin`.

### 2.3 Evaluation

For a principal `p`, a dataset `N` and an endpoint `E`, let `G(p, N, E)` be the grants of
`p` and of its roles that match `N` and apply to `E`. Unrestricted grants from C09's
`datasets` maps apply to every endpoint and cover every graph.

```
level(p, N, E)        = Admin if server-admin ∈ server(p)
                      = max { level(g) | g ∈ G(p, N, E) }        (None if empty)
level(p, N)           = max over every endpoint                  (C09's level)
read_graphs(p, N, E)  = ⋃ { graphs(g) | g ∈ G(p, N, E) }
write_graphs(p, N, E) = ⋃ { graphs(g) | g ∈ G(p, N, E), level(g) ≥ write }
```

The union of a grant without `graphs` with anything is "every graph". So one unrestricted
grant lifts every restriction at its level, which keeps grants monotonic. For example:

- Role `readers` gives `wiki = "read"` without restrictions, and user `carol` adds a
  `write` grant on `wiki` for `http://ex/carol/*`. Carol reads every graph and writes only
  her own.
- User `dave` has a `read` grant for `http://ex/public/*` and a `write` grant for
  `http://ex/dave/*`. Dave reads both sets and writes only the second, because a `write`
  grant also gives `read` on its graphs.

The **view** of a request is the pair `(read_graphs, write_graphs)` for its dataset and
endpoint. A view is **restricted** when either set is not "every graph" while its level
allows the access. Write graphs are always a subset of read graphs, because a `write`
grant also counts at `read`.

`level(p, N)` decides existence as in C09 §5.1. A dataset is visible when some grant
reaches `read` on it through some endpoint.

### 2.4 Tokens

C09 §2.3.1 intersects a token's scope with its owner's (or parent's) levels. This spec
keeps the scopes as they are and extends the intersection to endpoints and views:

```
level(t, N, E) = min(scope_level(N), level(owner, N, E))
view(t, N, E)  = view(owner, N, E), without write graphs when level(t, N, E) < write
```

A static token has its own grants, which may carry restrictions like a user's.

## 3. Configuration

### 3.1 Syntax

Every grantee of C09 §4.1 (anonymous, roles, users and static tokens) gains a list of
restricted grants named `grants`:

```toml
[[users]]
name = "carol"
password = "$argon2id$…"
datasets = { catalog = "read" }       # unrestricted, as in C09

[[users.grants]]
dataset = "wiki"
level = "write"
graphs = ["http://example.org/wiki/carol/*"]

[[users.grants]]
dataset = "wiki"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://example.org/wiki/public/*"]

[[roles.dashboards.grants]]
dataset = "metrics-*"
level = "read"
endpoints = ["query", "info"]

[[anonymous.grants]]
dataset = "public"
level = "read"
graphs = ["http://example.org/open/*"]
endpoints = ["query", "gsp-r", "info"]
```

A grant with neither `graphs` nor `endpoints` is the same as an entry of `datasets`.

### 3.2 Validation

Startup and reload (C09 §4.3) refuse a configuration when:

- `dataset` is not a valid dataset pattern;
- `level` is `admin` ("admin covers every graph and endpoint: grant it under datasets");
- `graphs` is empty ("omit graphs to cover every graph"), or an entry is neither the
  default graph's name nor an absolute IRI with optional `*`, or it names
  `urn:x-arq:UnionGraph`;
- `endpoints` is empty, or names an unknown endpoint;
- a grant names an unknown field (`deny_unknown_fields`).

The server logs a warning when an unrestricted grant of the same grantee already covers a
restricted one at its level, since the restriction then has no effect, and when a
wildcard pattern would have matched the inferred graph (§2.1).

### 3.3 Reload

Graph grants are part of the policy and reload with it on SIGHUP. In-flight requests keep
their view. The next request of every session and token uses the new rules, as for levels
in C09.

### 3.4 Mapping Fuseki's configuration

Fuseki's graph access control wraps a dataset in an `access:AccessControlledDataset`
whose `access:registry` holds `access:entry` items. Each entry names a user and a list of
graphs, either as a list `("user1" <g1> <g2>)` or with `access:user` and `access:graphs`.
`<urn:x-arq:DefaultGraph>` names the default graph. Fuseki applies graph access control
only to read-only datasets, and checks endpoints with `fuseki:allowedUsers` at the
server, the dataset and the endpoint, where every level must allow the user.

| Fuseki | Sparkles |
|---|---|
| `access:entry ("user1" <g1> <g2>)` on dataset `ds` | `[[users.grants]]` for `user1` with `dataset = "ds"`, `level = "read"` and `graphs = ["g1", "g2"]` |
| several entries for one user | several grants, or one grant listing every graph |
| `<urn:x-arq:DefaultGraph>` in an entry | `urn:x-arq:DefaultGraph` (or `default`) in `graphs` |
| a user without an entry sees no graph | a user without a grant does not see the dataset (404) |
| graph access only on read-only datasets | `read` grants behave the same. `write` grants with `graphs` are a Sparkles extension. |
| `fuseki:allowedUsers` on the dataset | `datasets = { ds = "read" }` (or `write`) for each user |
| `fuseki:allowedUsers` on one endpoint | a grant with `endpoints = ["query"]` (or another name) |
| `fuseki:allowedUsers "*"` (any authenticated user) | a role every user holds, or `[external] default_roles` |
| server-wide `allowedUsers`, which must also allow the user | not needed: a principal without any grant sees no dataset |

Fuseki's endpoint levels intersect, while Sparkles grants form a union. A Fuseki setup
that allows `user1` on a dataset and only `user2` on its update endpoint becomes a
`read` grant for `user1` and a `write` grant for `user2`. The union model cannot express
"user1 may use every endpoint except update" by subtraction, so the grant lists the
endpoints it allows instead.

## 4. The filtered dataset view

The engine applies a view where a query's RDF dataset is built: the default graph, the
named graphs, and the graphs each operator reads.

| A query's dataset | Restricted view |
|---|---|
| no `FROM`, no protocol dataset, store default graph | The default graph if the view covers it, else an empty default graph. |
| no `FROM`, store with a union default graph | The union of the named graphs the view covers. |
| the inference overlay (reasoning on) | The default graph and the inferred graph, each only if the view covers it. |
| `FROM <g>…` or `default-graph-uri` | The listed graphs the view covers. A hidden one behaves like a graph that does not exist. |
| `default-graph-uri=urn:x-arq:UnionGraph` | The union of the named graphs the view covers. |
| no `FROM NAMED` | The named graphs the view covers. |
| `FROM NAMED <g>…` or `named-graph-uri` | The listed graphs the view covers. |
| `GRAPH <urn:x-arq:UnionGraph> { … }` | The union of the named graphs of the query's dataset. |

The named graphs a view covers are found once per snapshot and rule set: the engine lists
the snapshot's graphs, matches their IRIs and keeps the result with the snapshot, so
repeated queries at one commit do not redo it.

Every operator reads graphs through the planner's graph filter, which is built from the
query's dataset. So these forms see only the view:

- `GRAPH ?g { … }` binds `?g` to visible graphs only, both when the planner binds it as a
  scan column and when it evaluates the group per graph;
- `GRAPH <hidden> { … }` matches nothing, the same as for a graph that does not exist;
- property paths filter every hop, because each edge lookup carries the graph filter;
- `COUNT(*)`, `COUNT(DISTINCT …)` and grouped counts answered from index counts or from
  statistics either read the visible graphs' counts (a single graph) or take the
  statistics' correction for graphs not read (§7.2);
- `text:query`, `spk:vectorSearch`, spatial searches and spatial joins apply the graph
  filter inside the search, before the top-k, as [F03](F03-full-text-search.md) and
  [F04](F04-vector-search.md) already require for `GRAPH` scopes;
- `DESCRIBE` reads the default graph of the view;
- `EXISTS`, `MINUS`, subqueries and `OPTIONAL` share the query's dataset.

`SERVICE` leaves the engine. A `SERVICE` call to the server itself is a new request
without the caller's credentials (C09 §3.5), so it runs as `anonymous`, and the outbound
policy refuses loopback addresses unless the operator allows them.

## 5. Reads

### 5.1 SPARQL, explain and history

- `/{ds}/sparql`, `/{ds}/query` and queries on `/{ds}` run on the view.
- `/{ds}/explain` plans on the view, with the redactions of §7.3.
- Point-in-time reads (`?at=`) apply the same view to the past state. The rules are the
  current ones, not the ones in force at that commit.

### 5.2 Graph Store Protocol

| Request | Restricted view |
|---|---|
| `GET ?graph=g` | The graph if the view covers it. A hidden graph answers 404 `no such graph`, the same as a missing one. |
| `GET ?default` | The default graph if the view covers it, else 404. |
| `GET` without a target (the whole dataset as quads) | The quads of the visible graphs only. This is also the export path for restricted principals. |
| `HEAD` | As `GET`. |

### 5.3 Search

- `text:query` and `/{ds}/text` (a `text:query` underneath) return hits of visible graphs
  only, and fill `limit` from them. Scores use BM25 statistics of the whole index, as
  F03 decided. This is accepted (§1.1).
- `spk:vectorSearch` returns the top k of the visible graphs.
- Spatial searches, spatial joins, nearest-neighbour search and `/{ds}/geo` read the
  visible graphs. Plans report no spatial candidate counts to a restricted principal,
  since the index counts candidates of every graph (§7.3).

### 5.4 Schema, statistics and dataset descriptions

| Route | Restricted view |
|---|---|
| `/$/schema/{ds}`, `/classes`, `/predicates`, VoID | Computed over the visible graphs. `graph=` and `declaredGraph=` name visible graphs, a hidden one answers like a missing one, and `union` is the union of the visible graphs. |
| `/$/stats/{ds}` | 403. The statistics count every graph. The schema routes give the same figures for the visible graphs. |
| `/$/datasets/{ds}` and listings | `quads`, the text index's document count, the spatial index's counts and the reasoning counts are left out. |
| `/$/reason/{ds}`, `/diagnostics` | 403. Diagnostics quote triples of every graph. |
| `/$/text/{ds}`, `/$/geo/{ds}`, `/$/vector/{ds}…` status and recall | 403. They count entries of every graph. |
| `/$/backups/{ds}` listings | 403. A backup holds every graph. |
| `/$/prefixes/{ds}`, `GET /{ds}/prefixes` | Unchanged. Prefixes are settings of the dataset, not graph data. |
| `/$/snapshots/{ds}`, `/$/history/{ds}`, `/$/validation/{ds}` reads | Unchanged. They hold names, commit references and settings. |
| `/$/ready/{ds}` | Unchanged. |
| `/$/tasks` | Tasks of datasets the caller sees through a restricted view are left out. |

### 5.5 Validation

`/{ds}/shacl`, `/{ds}/shex` and the MCP validation tools answer 403 to a restricted
principal. SHACL-SPARQL constraints and ShEx selectors run their own queries, and a
validation report can quote focus nodes of every graph that a shape reaches. Phase 2 can
validate a view once both validators take one.

A write-time guard (C10) still validates the whole dataset, so that a restricted writer
cannot get past it. Its rejection then answers 422 with the summary's conformance only:
results, focus nodes and the Turtle report are left out, because they can quote hidden
graphs.

### 5.6 Commits and diffs

- `/{ds}/diff` filters the net change to the visible graphs before it counts the changes
  against the budget, so the counts and the quads describe the view only. `graph=` names
  a visible graph, or answers like a graph with no changes.
- `/$/commits/{ds}` and `/$/commits/{ds}/{reference}` leave out the quad counts and the
  validation summary of each commit. Sequence numbers, times, kinds and messages stay.
- `Sparkles-Commit`, `ETag` and the commit numbers in results are unchanged. They reveal
  that a commit happened, not what it changed (§1.1).

### 5.7 MCP

Each tool call runs as the HTTP request's principal (C11 §4.9), with the view of the
tool's endpoint:

| Tool or resource | Endpoint | Restricted view |
|---|---|---|
| `list_datasets` | `info` | Listed, without quad counts. |
| `describe_schema` | `info` | Schema of the visible graphs. |
| `sparql_query`, `explain_query`, `describe_resource`, `search_text`, `similar_entities` | `query` | On the view. Plans as in §7.3. |
| `list_commits` | `info` | As §5.6. |
| `validate_shacl`, `validate_shex` | `shacl`, `shex` | Refused, as §5.5. |
| `sparql_update` | `update` | As §6. |
| dataset resources | `info` | As `describe_schema`. |

## 6. Writes

Every quad a write would insert or delete has its graph checked against the write graphs
**before** the store is changed, and the check looks at the quads the request asks for,
not at the ones that exist. A refused write changes nothing and answers 403
`write access to graph <g> of /ds required` (or `the default graph`). Because the check
precedes any lookup, `DELETE DATA` of a quad in a hidden graph fails the same way whether
or not the quad exists.

| Operation | Check |
|---|---|
| `INSERT DATA`, `DELETE DATA` | Every quad's graph. |
| `INSERT … WHERE`, `DELETE … WHERE`, `DELETE WHERE` | Constant template graphs before the WHERE runs. Graphs bound by variables per solution, before any quad is applied, whether or not the quad exists. |
| `WITH <g>` | `<g>` is the template's graph, so the same checks apply. The WHERE clause reads `<g>` only if the view covers it. |
| `USING`, `USING NAMED` | They set the WHERE clause's dataset, which the read view then filters. |
| `GRAPH <g>` in a template | As a constant template graph. |
| `LOAD <src>` | The default graph. `LOAD <src> INTO GRAPH <g>` checks `<g>`. Quads of a TriG or N-Quads source check their own graphs. |
| `CLEAR GRAPH <g>`, `DROP GRAPH <g>`, `CREATE GRAPH <g>` | `<g>`, before the graph is looked up. |
| `CLEAR DEFAULT`, `DROP DEFAULT` | The default graph. |
| `CLEAR NAMED`, `CLEAR ALL`, `DROP …` | They act on the view: the visible named graphs, and the default graph for `ALL`. Each must be writable. A hidden graph is left alone. |
| `ADD`, `MOVE`, `COPY` | Their source must be readable and their target writable, through the operations they are defined by. |
| GSP `PUT`, `POST`, `DELETE ?graph=g` or `?default` | The target graph, before the body is read. |
| GSP `POST` without a target (quads) | Every quad's graph. |
| GSP `PUT` or `DELETE` without a target | 403. They replace or clear the whole dataset, and a restricted principal does not see all of it. |
| `/{ds}/upload` | Every quad's graph. |
| `/{ds}/prefixes` writes | Need write graphs that cover every graph. Prefixes belong to the whole dataset. |

The store enforces the same rule a second time, in the write transaction. A write
transaction opened with a view refuses any insert or delete outside its write graphs.
That catches a write path that forgot the first check, at the cost of one map lookup per
quad, and only for restricted writers.

## 7. Caching, statistics and plans

### 7.1 The result cache

The result cache keys on the query's resolved dataset (its default and named graph ids)
as well as the plan and the commit. A restricted view resolves to explicit graph lists, so
it never shares an entry with an unrestricted query. Two principals with the same view
share entries, which is correct because they see the same data. No fingerprint of the
rules is needed.

### 7.2 Statistics shortcuts

The statistics shortcuts of the planner already answer for a set of graphs: a count over
one graph reads that graph's index counts, and a count over several graphs takes the
generation's statistics minus the quads of graphs not read and corrects for the delta
(`delta_statistics`). Those are exact, so they stay on for restricted views. The
shortcuts that need every graph (`COUNT(*)` from the size of the index) do not apply,
because the scan's graph filter is no longer "all".

### 7.3 Plans seen by a restricted principal

Plans are returned by `/explain`, with `explain=true` results and in the Sparkles result
format. For a restricted view, every plan has:

- its estimated rows and costs replaced by `-1`, since they come from statistics of every
  graph;
- the statistics notes in descriptions shortened to `[from statistics]`, without the
  number of quads of graphs not read;
- no operator counters (spatial candidates, refinements).

Actual row counts and timings stay, since they describe the view.

## 8. Endpoint permissions in the middleware

C09's middleware computes the need of a route. It now also computes the route's
endpoint, then decides:

| `level(p, N)` | `level(p, N, E)` | Response |
|---|---|---|
| None | any | As C09: 401 for anonymous, else 404. |
| ≥ read | < need, and `level(p, N)` < need | 403 as in C09 (`write access to /N required`). |
| ≥ read | < need, and `level(p, N)` ≥ need | 403 `the E endpoint of /N is not allowed`. |
| any | ≥ need | Pass. The handler computes the view for `E`. |

A principal that can reach `read` through some endpoint knows the dataset exists, so the
403 leaks nothing. `/$/datasets` lists datasets by `level(p, N)`, and `/$/whoami` gains
two fields:

```json
{ "datasets": { "wiki": "write", "public": "read" },
  "restricted": { "wiki": { "graphs": true, "endpoints": ["query", "info"] } } }
```

`restricted` names the datasets where the caller's grants limit graphs or endpoints. It
never lists the patterns, as C09 never lists dataset patterns.

## 9. Design sketch

**Engine (`crates/sparkles`).** A new module `access` holds:

```rust
pub struct GraphRule {
    pub default_graph: bool,
    pub iris: Vec<String>,     // exact names
    pub patterns: Vec<String>, // with `*`
    pub protected: Vec<String>,// matched only by exact names (the inferred graph)
}
pub enum Graphs { All, Only(GraphRule) }
pub struct GraphAccess { pub read: Graphs, pub write: Graphs }
```

- `QueryOptions::graphs: Option<Arc<GraphAccess>>`. `None` is every graph and costs one
  branch.
- `make_ctx` and the update's WHERE context intersect the `DatasetSpec` with the view
  after `FROM`, `USING` and the protocol dataset have been applied, and mark the context
  restricted. The planner maps `GRAPH <urn:x-arq:UnionGraph>` to the named set of a
  restricted context. Plans are redacted as §7.3 when the context is restricted.
- `WriteOptions::graphs` is checked by `WriteTxn::insert`, `delete` and `insert_bulk`
  before they look the quad up. `sparql::update` checks constant graph names before the
  writer lock is taken, variable ones per solution, and runs `CLEAR ALL` and `NAMED` on
  the view.
- `schema::SchemaOptions::graphs`, `store::DiffOptions::graphs` and a filter for
  `Snapshot` quad streams cover schema discovery, diffs and Graph Store reads.
- The resolved named graphs of a rule are kept with the snapshot's count cache.

**Server (`crates/sparkles-server`).**

- `auth::config`: the `grants` lists and their validation.
- `auth::Grants` gains the restricted grants. `Access` and `Principal` gain
  `level_at(ds, endpoint)` and `view(ds, endpoint) -> Option<Arc<GraphAccess>>`.
- `auth::routes`: `endpoint(route, method, uri, headers)` next to `need`, and the
  decision table of §8.
- `auth::restrict` takes the dataset and endpoint and sets `QueryOptions::graphs` and
  `WriteOptions::graphs`. The GSP, upload, schema, diff, commits, info and MCP handlers
  ask the principal for the view, and the handlers of §5.4 refuse a restricted one.
- Without the `auth` feature, `Principal::view` is always `None`.

## 10. Acceptance examples

**Fixture.** A dataset `wiki` holds the default graph, `http://ex/a/1`, `http://ex/a/2`,
`http://ex/b/1` and the inferred graph, each with a few quads that share subjects and
predicates, plus a full-text index and vectors. The users are:

- `full`: `wiki = "write"`;
- `ra`: `read` on `http://ex/a/*`;
- `rad`: `read` on `default` and `http://ex/a/*`, `write` on `http://ex/a/1`;
- `ep`: `wiki = "read"` limited to `endpoints = ["gsp-r"]`.

**Reads.**

- **B1. Differential.** For every query of a fixed list, the result as `ra` equals the
  result on a dataset that holds only `ra`'s graphs, both after a bulk load and with a
  delta. The list covers `GRAPH ?g`, `FROM` and `FROM NAMED` of hidden graphs,
  `GRAPH <hidden>`, the union graph IRI, property paths, `COUNT(*)` with and without a
  pattern, `COUNT(DISTINCT)`, grouped counts by class and by predicate, `DESCRIBE`,
  `EXISTS`, `MINUS`, `text:query`, `spk:vectorSearch` and a spatial filter.
- **B2. Full access.** `full`'s results, plans and cache keys equal those of the same
  query without auth.
- **B3. Existence.** As `ra`, `ASK { GRAPH <http://ex/b/1> {} }` and the same for a graph
  that does not exist both answer false. GSP `GET ?graph=http://ex/b/1` and a missing
  graph answer identical 404s.
- **B4. Plans.** As `ra`, `/explain` of a count over every graph shows `-1` estimates and
  no unread-quad count.
- **B5. Cache.** `full` runs a query, then `ra` runs it: `ra`'s answer is its own, not
  the cached one, and the reverse.
- **B6. Routes.** As `ra`: `/$/stats/wiki` → 403; `/$/schema/wiki?graph=union` counts
  `ra`'s graphs only; `/$/datasets/wiki` has no `quads`; `/{ds}/diff` shows only changes
  of `http://ex/a/*`; `/$/commits/wiki` has no counts.

**Writes.**

- **B7.** As `rad`: `INSERT DATA { GRAPH <http://ex/a/1> {…} }` → 200;
  `GRAPH <http://ex/a/2>` → 403; `DELETE DATA` of an existing quad of `http://ex/b/1` and
  of one that does not exist → identical 403s; the head is unchanged.
- **B8.** As `rad`: `INSERT { GRAPH ?g { … } } WHERE { BIND(<http://ex/b/1> AS ?g) }` →
  403. `DELETE WHERE { GRAPH ?g { ?s ?p ?o } }` → 403 because `http://ex/a/2` is visible
  and not writable. `CLEAR ALL` → 403 for the same reason, and for a principal that may
  write every visible graph it clears them and leaves `http://ex/b/1`.
- **B9.** As `rad`: GSP `PUT ?graph=http://ex/a/1` → 2xx; `PUT ?graph=http://ex/b/1` →
  403; `POST` of N-Quads with one quad in `http://ex/b/1` → 403 with nothing written;
  `PUT` without a target → 403. Upload behaves the same.
- **B10.** `LOAD <file> INTO GRAPH <http://ex/b/1>` as `rad` → 403.

**Endpoints.**

- **B11.** As `ep`: `GET /wiki/get?default` → 200; `/wiki/sparql` → 403
  `the query endpoint of /wiki is not allowed`; `/secret/sparql` → 404 as before.
- **B12.** MCP as `ra`: `sparql_query` returns `ra`'s rows; `validate_shacl` is refused.

**Configuration.**

- **B13.** A grant with `level = "admin"` and `graphs`, an empty `graphs`, an unknown
  endpoint or `urn:x-arq:UnionGraph` fails validation with a message naming the grantee.

## 11. Rejected alternatives

- **Filtering in the HTTP handlers.** The handlers would have to know every operator that
  reads data, and every new endpoint would need its own filter. The engine already
  carries a graph filter on each scan.
- **Disabling the fast paths for restricted principals.** It would be simpler but slower,
  and unnecessary: the statistics shortcuts are exact for a set of graphs.
- **Keeping the result cache off for restricted principals.** The resolved dataset in the
  key already separates views, and principals with the same view may share entries.
- **Checking writes against the changes that took effect.** A delete that matched nothing
  would succeed for a hidden graph that lacks the quad and fail for one that has it,
  which reveals the quad.
- **Making `CLEAR ALL` fail for every restricted principal.** It acts on what the caller
  sees, as the rest of the view does.
- **Deny rules or per-graph exceptions.** Grants stay a union for the reasons of C09 §14.
- **Solid WAC ACL documents per graph.** They need WebIDs and per-resource discovery, and
  would split the policy between the configuration and the data.
- **A Fuseki assembler parser for `access:` vocabularies.** The mapping table of §3.4 is
  short, and a second configuration language would need its own validation and reload.

## 12. Open questions

1. Should token scopes narrow graphs too, so that a CI token can be limited to one
   tenant's graph?
2. Should the validation endpoints validate the view (§5.5) instead of refusing?
3. Should `/$/stats` answer with the visible graphs' counts instead of 403?
4. Should an operator be able to let wildcards cover the inferred graph?

## 13. Sources

- **Sparkles repository:** `crates/sparkles/src/sparql/` (`mod.rs`, `ctx.rs`, `plan.rs`,
  `exec.rs`, `stats.rs`, `cache.rs`, `update.rs`), `store.rs` (`WriteTxn`), `store/diff.rs`,
  `schema.rs`, `validation.rs`, and `crates/sparkles-server/src/` (`auth/`, `http.rs`,
  `http/`, `mcp/`); the specs C09, C10, C11, F03, F04 and F06.
- **Apache Jena Fuseki documentation** (Apache-2.0): "Data Access Control for Fuseki",
  fetched 2026-10-02,
  https://jena.apache.org/documentation/fuseki2/fuseki-data-access-control.html. Used for
  `access:AccessControlledDataset`, `access:registry`, `access:entry` in list and
  property form, `urn:x-arq:DefaultGraph` in entries, graph access control being limited
  to read-only datasets, and `allowedUsers` at server, dataset and endpoint levels with
  `"*"` meaning any authenticated user.
- **W3C Solid** Web Access Control and Access Control Policy (access modes, the absence
  of deny in WAC): cited from working knowledge.
- **Role-based access control** as in the NIST RBAC model (roles as unions of
  permissions): cited from working knowledge.
- **SPARQL 1.1** Query (RDF datasets, `FROM`, `FROM NAMED`, `GRAPH`), Update (`WITH`,
  `USING`, `CLEAR`, `DROP`, `LOAD`, `ADD`, `MOVE`, `COPY`), Protocol and Graph Store
  Protocol: cited from working knowledge.
- **Not consulted:** anything from Fluree, including its policy language, source,
  documentation and design notes.

## Outcome

**Delivered on 2026-10-02**, as one phase.

- The engine's `sparkles::access` module holds `GraphRule`, `Graphs` and `GraphAccess`.
  `QueryOptions::graphs`, `WriteOptions::graphs`, `SchemaOptions::graphs` and
  `DiffOptions::graphs` carry a view. `DatasetSpec::restrict` intersects a query's dataset
  with it after `FROM`, the protocol dataset, the inference overlay and `USING` are
  applied, and the planner's graph filter does the rest. The visible named graphs are
  kept per snapshot and rule in the snapshot's count cache.
- The server's grants gained the `grants` lists of §3.1, `Endpoint`, and
  `Principal::level_at`, `view` and `limits`. The middleware computes each route's
  endpoint next to its need, and refuses the whole-dataset routes of §5.4 for a limited
  view. `auth::restrict` sets the view on query, update, explain, text and MCP options.
  The Graph Store, uploads, schema, diffs, commits, snapshots, readiness, tasks,
  `/{ds}/geo` and the MCP tools apply it themselves.

**Deviations from the design.**

- `DatasetInfo` keeps `quads` for a limited caller, counting the quads of the visible
  graphs, so that listings keep their shape. MCP's `list_datasets` does the same.
- `/$/history/{ds}` (GET) is refused like the other whole-dataset routes, because it
  reports the size of the retained history. Snapshot listings leave out the pinned
  commit's counts.
- A caller that some endpoint does not admit gets an empty view there, so the redactions
  of §5.4 and §5.6 apply to every caller whose grants limit graphs or endpoints.
- When a view's patterns cover every named graph the snapshot has, the query keeps the
  store's own named graphs and union instead of an explicit list, so it runs the plans of
  the full dataset. The answers are the same, since no graph is hidden.
- In a store with `--union-default-graph`, `DESCRIBE` without a view reads the stored
  default graph as well as the named graphs, while a view's union holds the visible named
  graphs only. That older difference between `DESCRIBE` and the other query forms was
  left as it was.
- `/{ds}/text` without `graph=` searches the default graph, as before, so a caller who
  cannot see the default graph gets no hits there.

**Tests at landing.**

- `crates/sparkles/tests/graph_access.rs` checks that 47 queries through five views give
  exactly the answers of a store that holds only the view's graphs. The queries cover
  `GRAPH ?g`, `FROM` and `FROM NAMED` of hidden graphs, the union graph, property paths,
  counts with and without patterns, grouped counts, `DESCRIBE`, `CONSTRUCT`, `EXISTS`,
  `MINUS`, `OPTIONAL`, subqueries, string filters, `spk:vectorSearch`, `text:query` and
  `geof:sfWithin`. The stores hold a compacted base with a delta on top, with and without
  a union default graph, and an unrestricted run fills the result cache first. The same file
  covers protocol datasets, the inference overlay, plan redaction, views that leave plans
  unchanged, the write rules of §6, loads and replaces, diffs and schema reports.
- `sparql::access_tests` runs class, predicate and distinct counts through views with the
  statistics' work limit lifted, so that the statistics shortcuts and their correction for
  graphs not read answer them, and compares them with a store of the view's graphs.
- `router_tests::auth::graphs` is a matrix of five users (full, graph-limited reader,
  graph-limited writer, endpoint-limited, and mixed) against queries, the Graph Store,
  updates, schema, explain, full-text search, diffs, commits, listings, whoami and the
  refused routes, and validates configurations. `router_tests::mcp` adds the MCP tools.
- Crate and server tests, Clippy over all targets, `mise run lint:features`, the W3C
  suites (SPARQL 1.0 482/482, 1.1 query 328/328, 1.1 update 157/157, 1.2 269/269) and the
  SHACL suites pass. `mise run ci` passes.

**Performance.** Measured over HTTP on 1M quads, with 100k in the default graph and 45k
in each of 20 named graphs, as medians of 21 runs that alternate between callers. The
machine was shared with other builds, so differences under about 10% are noise.

| Query | `main`, full grant | This change, full grant | View of the default graph and 18 named graphs |
|---|---|---|---|
| `COUNT(*)` of the default graph | 0.59 ms | 0.60 ms | 0.62 ms |
| `COUNT(*)` over `GRAPH ?g` | 12.4 ms | 12.3 ms | 15.4 ms |
| class counts over `GRAPH ?g` | 5.4 ms | 5.4 ms | 5.5 ms |
| counts per graph | 23.3 ms | 22.6 ms | 24.4 ms |
| a star join in `GRAPH ?g` | 2.0 ms | 2.0 ms | 2.0 ms |
| a `STRSTARTS` filter in `GRAPH ?g` | 1.0 ms | 1.1 ms | 1.1 ms |
| a property path | 1.0 ms | 0.9 ms | 0.9 ms |
| one subject in `GRAPH ?g` | 0.8 ms | 0.8 ms | 0.8 ms |

A caller with a full grant runs exactly as before: its options carry no view, and the
tests check that its plans do not change. A limited view costs up to a quarter more on
scans of every named graph, where each row's graph is looked up in the visible set. Joins,
filters, paths and lookups show no difference. A view whose patterns cover every existing
graph measured the same as a full grant once the change above was made.

**Read paths merged later.** The change feed (`/{ds}/changes`, in JSON, RDF Patch and
server-sent events), RDF Patch diffs, warm snapshot pins and `spk:hybridSearch` arrived on
`main` while this was built. They follow the view as well:

- `store::ChangesOptions::graphs` filters each commit's changes in the log walk and in
  state comparisons, before they count against the page's limit. The feed belongs to the
  `diff` endpoint. Every commit is still listed, so a commit of hidden graphs only shows
  no changes, and a commit too large to list has no change or quad counts.
- RDF Patch diffs are written from the filtered diff, so a patch leads from one state of
  the view to the next.
- Warm pins only keep a pinned state materialized; reads at a pinned snapshot are queries,
  which take the view.
- `spk:hybridSearch` plans its text and vector searches with the active graph's filter,
  so the fusion ranks visible hits only. The differential test includes it.

**Not built.** Graph restrictions on token scopes, validation of a view (§5.5), and a
`/$/stats` answer for the visible graphs remain open questions 1–3. Full-text scores
still use the statistics of the whole index.
