# Server and security capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Server and protocols

The server speaks Fuseki's protocols and admin API, and adds GraphQL, stored queries and
an OpenAPI description. The MCP server for agents is described under
[AI and agents](../ai/README.md#mcp-server).

### SPARQL protocol and Fuseki compatibility

* The server implements the SPARQL protocol, the Graph Store Protocol, upload, the `/$/`
  admin API for datasets, stats, compaction, backups and tasks, and Jena's special graphs
  `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph`.
* A GET without a query at `/{ds}/sparql` or `/{ds}/query` returns a SPARQL 1.1 Service
  Description ([API.md](../API.md#service-description)). It describes the services, their
  languages, formats and features, every extension function, aggregate and property
  function, the entailment regime and the default dataset, and links the VoID
  description. Named graphs and counts follow the caller's grants.
* The admin API has Fuseki's `/$/backups` alias, `/$/backups-list`, `/$/stats` with
  request counters, `?state=offline|active`, `deleteOld`, the `/$/validate/*` services and
  assembler bodies on `POST /$/datasets`. Dataset, task and server JSON carry Fuseki's
  members.
* The protocol supports `using-graph-uri`, `?graph=union`, `?target=` on `/shacl`, direct
  Graph Store naming with `--gsp-direct-naming`, `output=`, `results=` and `force-accept`,
  and Jena's RDF Thrift, RDF Protobuf, RDF/JSON, TriX and SPARQL Results Thrift formats.
* `mise run test:jena-clients` tests Jena's own clients against the server. These are
  `RDFConnectionRemote`, `RDFConnectionFuseki`, `GSP`, `DSP`, `QueryExecHTTP`,
  `UpdateExecHTTP` and Jena's RDF Patch writers.
* `sparkles config import fuseki` converts a Fuseki installation
  ([G08](../specs/G08-fuseki-configuration.md)). It reads a `config.ttl`, the service files
  of `run/configuration/` and `shiro.ini` or the `fuseki:passwd` file. It writes a `serve`
  script, a data migration script with `tdb2.tdbdump` steps, an auth configuration with
  argon2id hashes and grants, and each dataset's text, spatial, RDFS and inference
  settings. A report marks each element as converted, approximated, manual, ignored or
  unsupported. `serve --fuseki-config` converts at each start and loads `ja:data` into
  memory, as Fuseki does.
* `/$/openapi.json` and `/$/openapi.yaml` serve an OpenAPI 3.1 description of the whole
  HTTP API, which `sparkles openapi` prints and `docs/openapi.json` checks in
  ([X03](../specs/X03-openapi-and-completions.md), [API.md](../API.md#openapi-description)). A
  test keeps it equal to the server's route table.
* The query result cache has `--result-cache-mb`, `nocache=true`, statistics in
  `/$/stats` and `POST /$/cache/clear/{ds}`.

### GraphQL

* Each dataset can serve read-only GraphQL at `/{ds}/graphql`, configured through
  `/$/graphql/{ds}` and `sparkles graphql` and implemented in the crate
  `sparkles-graphql` ([API.md](../API.md#graphql)).
* An administrator installs a mapping schema in GraphQL SDL whose `@rdf` directives name
  classes, predicates and enum IRIs, with `@prefix`, `@lang` and `@single`. Drafts come
  from the write-time guard's SHACL shapes or from the shapes the data supports.
* The server generates lookups, cursor connections with `totalCount`, `node(id:)`,
  filters with `and`, `or`, `not` and nested object filters, and orders with an id
  tie-break.
* A request runs as a fixed number of SPARQL queries on one snapshot, and its cursors name
  their commit. Depth, fetch groups and estimated nodes are limited before a request runs.
  Every query runs with the caller's graph view and protections of triples.
* Introspection, the GraphQL over HTTP media types and status codes, and execution errors
  with null propagation follow the specifications.

### Stored queries

* Named, parameterized queries are stored per dataset in `queries.json`, which backups
  and clones include ([API.md](../API.md#stored-queries)). `/$/queries/{ds}`,
  `/{ds}/queries/{name}`, `sparkles queries` and `sparkles::stored` manage and run them.
* Parameters are typed, with defaults and allowed values, and each query has a
  description, example questions and a default result format.
* A run binds each value as one RDF term through initial bindings, never as query text,
  and goes through the same path as `/{ds}/sparql`, with the caller's view, budgets and
  rate limits.
* Every change is a version with its author, message, the dataset's head commit and a
  chained digest, and `If-Match` makes changes conditional. The MCP server offers each
  stored query as a tool.


## Access control and security

Authentication is off by default. Once it is on, access is denied unless a grant allows it.

* `serve --auth-config` turns on authentication and per-dataset access control
  ([API.md](../API.md#authentication-and-access-control),
  [C09](../specs/C09-dataset-access-control.md)). The access levels are `read`, `write` and
  `admin`, granted by dataset name or pattern, plus `metrics`, `federate` and
  `server-admin`. Hidden datasets answer `404`.
* Callers sign in as HTTP Basic users with argon2id hashes, with API tokens, with OIDC in
  the UI, or through a trusted forward-auth proxy. API tokens are scoped, expiring and
  revocable, hashed at rest, and never have more access than their owner.
* OIDC sign-in for the UI is native, using the authorization code flow with PKCE, with
  back-channel logout and optional idle timeouts. API clients can send the provider's JWT
  access tokens, checked against its JWKS with key rotation, and Cloudflare Access
  assertions are verified. Groups map to roles.
* Requests that overlap a policy or credential change may receive `403` and must
  retry with current credentials. Token revocation, reissue, session logout and
  stored group changes invalidate prepared authentication. A password verification
  that finishes after a reload cannot restore the previous grants in the cache
  or create a session from them. Last-used bookkeeping does not revoke access.
* Dataset and branch administration emit INFO records under `sparkles::audit` with the
  existing actor and completed operation identities. Clones record the selected source
  commit after registration, and authorized failures record bounded codes. Continuing
  mutation workers retain these records when an HTTP request is cancelled. The
  [operations guide](../operations/README.md#observability) describes local commands and
  automatic scratch expiry.
* With an OIDC provider and `serve --mcp`, `/$/mcp` is an OAuth 2.1 protected resource
  ([API.md](../API.md#oauth-for-the-mcp-endpoint)). It publishes RFC 9728 metadata that
  names the provider, accepts access tokens whose audience is its own resource
  identifier at `/$/mcp` only, and answers a token that lacks a scope with an
  `insufficient_scope` challenge. The scopes `sparkles:read`, `sparkles:update` and
  `sparkles:admin`, optionally per dataset, only narrow the user's grants.
* `serve --tls-cert --tls-key` serves HTTPS with rustls, HTTP/2 through ALPN and
  certificate reload. Cookies follow CSRF and CORS rules. `sparkles auth login` signs the
  CLI in with a browser loopback or a device code.
* A grant can cover only some named graphs of a dataset, only some endpoints, or both
  ([API.md](../API.md#graph-level-access-control), [C12](../specs/C12-graph-access-control.md)).
  Each query runs on a filtered view, so every query form, search, schema report, diff,
  change feed and Graph Store request sees only the covered graphs, and a hidden graph
  behaves like a missing one. Writes outside the writable graphs fail before anything
  changes.
* Protections of triples hide triples by predicate, by subject class, by graph, or by a
  SPARQL pattern with the caller's name, roles and groups bound
  ([API.md](../API.md#protections-of-triples), [C12b](../specs/C12b-triple-access-control.md)).
  Grants lift protections by name. Every read path, including statistics, paths, search,
  `DESCRIBE`, schema reports, diffs, stored queries, MCP tools and RDFS on read, sees only
  the visible triples, and writes of protected triples fail.
* Callers with access to every graph and no protections in force keep the same plans and
  caches.
* Rate limiting is off by default. Each request class gets per-client GCRA buckets and
  concurrency caps, with per-dataset overrides, and a limited request gets `429` or `503`
  with `Retry-After` and `RateLimit` headers. SIGHUP reloads the configuration. With
  auth, a `preauth` stage limits failed credential checks per address and per IPv6 /48
  before any password is hashed.
* Outbound requests from SERVICE, LOAD, imports, embeddings, model providers and
  notifications go through one
  [outbound network policy](../USAGE.md#outbound-requests-service-and-load).

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Server, access control and operations

* The JSON metrics snapshot and OTLP export omit some produced-row, memory-peak and
  rebuild series available in Prometheus. Cross-format parity is roadmap item 342.
* There are no JVM metrics. Assembler bodies are read only for the part that maps to a
  dataset. Direct Graph Store naming is chosen per dataset at start with
  `--gsp-direct-naming` and not through the settings API. Roadmap item 76.
* There is no ontology object API. Roadmap item 71.
* The change feed refuses callers whose protections depend on the data, and protections
  hide triples, not the IRIs that visible triples mention. Roadmap item 75.
* GraphQL has no mutations, subscriptions, stored GraphQL queries, `persistedOnly` or UI
  page. Nested lists are not connections, and a nested `first` limits the response while
  the engine still produces every value of the field. Parent ids reach a child query as
  terms rather than engine ids, and blank node parents each take a `UNION` branch.
  Roadmap item 37.
* Stored queries have no stored updates, no parameters bound to several values at once,
  and no private queries per user. Roadmap item 39.
* Sparkles runs as a single node on local disk. There is no clustering and there are no
  replicas, roadmap item 66. Live RDF data on local disk is not encrypted at rest, item 65.
* Backups cover individual datasets, with no whole-server backup, roadmap item 61.
  Retained-history backups and storage-retention controls also remain under item 61.
  Encryption keys for backup repositories cannot be managed through the API, Vault or a
  cloud KMS, and write-only age-recipient repositories are not built, item 65.
* Without `--secrets-key`, runtime keys are sealed with a key that the server generated in
  the data directory, which does not protect them from someone who can read that
  directory. An operator who wants no keys on disk locks the secrets in the settings file.
  Roadmap item 77.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Outbound requests (SERVICE and LOAD)](../USAGE.md#outbound-requests-service-and-load).
* Read [Service description](../API.md#service-description).
* Read [OpenAPI description](../API.md#openapi-description).
* Read [GraphQL](../API.md#graphql).
* Read [Stored queries](../API.md#stored-queries).
* Read [Authentication and access control](../API.md#authentication-and-access-control).
* Read [OAuth for the MCP endpoint](../API.md#oauth-for-the-mcp-endpoint).
* Read [Graph-level access control](../API.md#graph-level-access-control).
* Read [Protections of triples](../API.md#protections-of-triples).

## Roadmap

The remaining work described above refers to [item 37](../ROADMAP.md), [item 39](../ROADMAP.md), [item 61](../ROADMAP.md), [item 65](../ROADMAP.md), [item 66](../ROADMAP.md), [item 71](../ROADMAP.md), [item 75](../ROADMAP.md), [item 76](../ROADMAP.md), [item 77](../ROADMAP.md), [item 342](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.

### Read-view preparation limits

Rust callers can prepare graph, predicate and class permission masks with
`GraphAccess::masked_controlled` and one `Preparation`. Its `Control` carries
cancellation and an absolute deadline through binding, scans, vocabulary decoding,
class closure, ordering and supported providers. The request memory cap covers
conservative temporary peaks and retained delta, policy and mask storage. Cached
results must fit it too. `with_no_wait(true)` refuses a preparation lock that is held.
A refused call returns no partial view.

The returned `PreparedView` owns its retained-state reservations and borrows its access
policy and snapshot to callers. Apply both together, since the physical triple mask
does not enforce graph restrictions by itself. Keep that owner alive while using the
prepared view. A snapshot that another policy already masked is refused unless the
requested policy hides no triples.
Independent copies need their own accounting. This cap does not cover subsequent
query execution or replace the store's accounting for immutable mappings and shared
caches. Existing query, hidden-quad and pattern-row limits retain their scope.

Providers implement `GuardDecision::exempt_controlled` to share these controls and
reservations. The default returns `Unsupported` without running the legacy callback.
The built-in GraphRAG disclosure guard supports controlled preparation, including cold
index reads, footprint digest checking and exact coverage. It builds a temporary local
index on a cold call and can reuse an existing compatible index after admission.

Arbitrary protection-pattern planning and unresolved providers nested in triple rules
or protection graph definitions remain unsupported by the bounded helper. Static
nested guards remain supported.
Guarded Dataset queries, C25 exact references and U02 readers still need adoption under
[C29](../specs/C29-bounded-view-preparation/README.md), roadmap item 395. These helpers
provide no managed-write authority, source authenticity or complete HTTP query
preparation limit. Bounded callers must propagate refusal without weakening the policy
or selecting another state.

Controlled dictionary lookup and mask preparation incur additional checks and temporary
allocations. Delta vocabulary lookup may scan retained entries, and a cold disclosure
call rebuilds its local index. These costs belong to the opt-in preparation helper.
[Item 398](../ROADMAP.md) tracks release measurements and reductions before broader
caller adoption.
