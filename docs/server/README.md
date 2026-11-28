# Server and security

Sparkles serves Apache Jena Fuseki's HTTP protocols and administration API alongside read-only GraphQL, versioned stored queries, a DCAT 3 resource catalog, and layered access control over datasets, graphs, and triples. The [capability matrix](../FEATURES.md#capability-status) records delivery status for each area.

| Section | What it covers |
|---|---|
| [Server and protocols](#server-and-protocols) | SPARQL, Graph Store, Fuseki admin compatibility, OpenAPI, GraphQL, stored queries, and the resource catalog. |
| [Access control and security](#access-control-and-security) | Authentication methods, OAuth for MCP, TLS, graph/triple protections, rate limiting, and bounded view preparation. |
| [Configuration and usage](#configuration-and-usage) | Links to protocol, auth, GraphQL, and catalog guides. |
| [Known gaps](#known-gaps) | Missing ARQ/Fuseki items, GraphQL and catalog limits, and single-node/encryption boundaries. |

## Server and protocols

The server speaks Fuseki's SPARQL, Graph Store, and `/$/` administration protocols and adds GraphQL, stored queries, a resource catalog, and an OpenAPI 3.1 schema. The [MCP server for agents](../ai/README.md#mcp-server) is covered in the AI and agents guide.

### SPARQL protocol and Fuseki compatibility

* **Standard protocols and admin routes.** The server implements the SPARQL 1.1/1.2 Protocol, the Graph Store Protocol, file uploads, Jena's `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph` special graphs, and Fuseki's `/$/` admin API (`/$/datasets`, `/$/backups`, `/$/backups-list`, `/$/stats` with request counters, `/$/compact` with `deleteOld`, `/$/tasks`, `/$/server`, `/$/ping`, `/$/metrics`, `/$/validate/*`, `?state=offline|active`, and assembler payloads on `POST /$/datasets`). Dataset, task, and server JSON responses include Fuseki's expected fields.
* **Service Description.** Sending `GET` without a query to `/{ds}/sparql` or `/{ds}/query` returns a SPARQL 1.1 Service Description listing supported languages, formats, extension functions, aggregates, property functions, entailment regime, default dataset, and VoID link, filtered by caller grants ([API.md](../API.md#service-description)).
* **Protocol options and formats.** Supports `using-graph-uri`, `?graph=union`, `?target=` on `/shacl`, direct Graph Store naming (`--gsp-direct-naming`), `output=`, `results=`, `force-accept`, and Jena's RDF Thrift, RDF Protobuf, RDF/JSON, TriX, and SPARQL Results Thrift formats. `mise run test:jena-clients` exercises Apache Jena's `RDFConnectionRemote`, `RDFConnectionFuseki`, `GSP`, `DSP`, `QueryExecHTTP`, `UpdateExecHTTP`, and RDF Patch writers against the server.
* **Fuseki configuration migration.** `sparkles config import fuseki` translates `config.ttl`, `run/configuration/` service files, and `shiro.ini` or `fuseki:passwd` into a `serve` script, a `tdb2.tdbdump` data migration script, argon2id auth configuration, and dataset text, spatial, RDFS, and inference settings, reporting each item as converted, approximated, manual, ignored, or unsupported ([G08](../specs/G08-fuseki-configuration.md)). Running `serve --fuseki-config` converts on startup and loads `ja:data` into memory as Fuseki does.
* **OpenAPI and caching.** `/$/openapi.json` and `/$/openapi.yaml` (and `sparkles openapi`, checked against `docs/openapi.json` in tests) publish an OpenAPI 3.1 description of the full HTTP API ([X03](../specs/X03-openapi-and-completions.md), [API.md](../API.md#openapi-description)). Query result caching is controlled by `--result-cache-mb`, `nocache=true`, `/$/stats`, and `POST /$/cache/clear/{ds}`.
* **Stream timeouts.** Finite streamed query, Graph Store, diff, and change responses enforce a 60-second idle limit (`--stream-idle-timeout` or `SPARKLES_STREAM_IDLE_TIMEOUT`) that counts only while ready output goes unread, plus an optional total transfer ceiling (`--stream-transfer-timeout` or `SPARKLES_STREAM_TRANSFER_TIMEOUT`). Expiry truncates the response and requests query cancellation while producer ownership remains until execution stops ([USAGE.md](../USAGE.md)).
* **Hybrid-CPU scheduling.** On Linux hosts with Intel hybrid processors, request work stays pinned to performance cores while capacity permits ([USAGE.md](../USAGE.md#threads-on-hybrid-processors)).

### GraphQL

Each dataset can expose a read-only GraphQL endpoint at `/{ds}/graphql`, backed by `sparkles-graphql` and managed through `/$/graphql/{ds}` and `sparkles graphql` ([API.md](../API.md#graphql)).

* **SDL mappings and drafts.** Administrators install a GraphQL SDL schema annotated with `@rdf`, `@prefix`, `@lang`, and `@single` directives mapping types and fields to classes, predicates, and enum IRIs. Initial drafts can be generated from the dataset's write-time SHACL shapes or from discovered data shapes.
* **Generated operations and batched execution.** The server generates entity lookups, cursor connections with `totalCount`, `node(id:)`, boolean filters (`and`, `or`, `not`, and nested object filters), and deterministic ordering with id tie-breaks. Each request compiles into a fixed number of SPARQL queries over one snapshot, pinning cursors to that commit. Depth, fetch groups, and estimated nodes are bounded before execution, and every query enforces the caller's graph and triple access controls.
* **Protocol conformance.** Supports GraphQL introspection, standard HTTP media types and status codes, and spec-compliant execution errors with null propagation.

### Stored queries

Named, parameterized SPARQL queries live per dataset in `queries.json` (preserved by backups and clones) and are managed through `/$/queries/{ds}`, `/{ds}/queries/{name}`, `sparkles queries`, and `sparkles::stored` ([API.md](../API.md#stored-queries)).

* **Typed parameters and safe binding.** Parameters declare types, defaults, and allowed values alongside a description, example questions, and default output format. Parameters bind as RDF terms through initial bindings rather than text substitution, and execution uses the same path, visibility rules, budgets, and rate limits as `/{ds}/sparql`.
* **Versioning and MCP exposure.** Every edit creates a version recording author, message, head commit, and chained digest, with `If-Match` concurrency checks. The MCP server exposes each stored query as a tool.

### Resource catalog

The server maintains a catalog of external sources, vocabularies, and datasets in `resources.json` inside its data directory, managed by server administrators at `/$/resources` ([API.md](../API.md#resource-catalog), [C30](../specs/C30-resource-catalog/README.md)). The catalog stores metadata rather than raw RDF data.

* **Immutable versions and DCAT 3 metadata.** Resource versions are immutable once registered. Imports and dataset graph sources pin existing versions, preventing deletion of referenced resources or versions. The catalog exports as JSON or any RDF syntax using DCAT 3, PROV-O derivations, SPDX checksums and licences, VANN namespaces, and VoID graph locations.
* **Licence tracking and obligation propagation.** SPDX expressions classify into open, attribution, share-alike, and restricted tiers, with missing licences forbidding redistribution. Obligations propagate through imports and graph sources to each dataset graph, generating a `NOTICE` file. Restricted terms require recorded administrator acceptance. The licence gate warns by default and refuses only under `serve --license-policy enforce`. Export and publication routes do not consult the licence gate.
* **Vocabulary namespaces.** Each vocabulary owns its namespaces and prefixes uniquely within the server. Vocabulary headers can be read from dataset graphs and compared against the catalog, and one request installs a vocabulary's prefixes and imported prefixes into a dataset.

### Linked-data publishing

The linked-data front end resolves IRIs under hosted vocabulary namespaces and configured version IRIs from the resource catalog ([API.md](../API.md#linked-data-publishing), [C32](../specs/C32-linked-data-publishing/README.md)).

* **Content negotiation and redirects.** Requests to `/$/ld`, `/$/ld/ns/{prefix}` and `/$/ld/ns/{prefix}/{*local}` negotiate `text/html`, `text/turtle`, `application/ld+json`, `application/rdf+xml` and `application/n-triples` with `Vary: Accept`, answering `303 See Other` to `/$/ld/doc` for slash IRIs and `200 OK` directly for hash IRIs and hash namespace documents. Requests to `/$/ld/doc` return the negotiated representation directly with `200 OK`.
* **Versioned snapshots and recipes.** Catalog versions that set `versionIri` and `snapshot` resolve versioned IRIs to their pinned snapshots or commits with Memento headers. Administrators can export Apache `.htaccess`, w3id and PURL redirection recipes at `/$/resources/{id}/recipes`.
* **Authorization and safety.** Every lookup, redirect target, HTML page and bounded `DESCRIBE` response runs through the caller's dataset, graph and triple read permissions and server query budgets. Unauthorized or missing IRIs answer `404 Not Found` without leaking private terms, and HTML pages escape all dynamic values under a script-free Content Security Policy.

* **Ontology documentation.** Open `/$/ontology/{ds}/doc` for a dataset or `/$/ld/resources/{id}/doc` for a catalog resource. The generated HTML groups asserted hierarchy, definitions, usage and examples and exports as one offline file.
* **Catalog search and OLS4.** Search readable resources by label, synonym, definition or identifier with lexical or character trigram vector ranking, ontology facets and autocomplete. The OLS4 REST subset shares the same caller-visible data and serves ontology and term browsing under `/ols4/api`.

## Access control and security

Authentication is disabled by default. When enabled via `serve --auth-config`, access is denied unless explicitly granted ([API.md](../API.md#authentication-and-access-control), [C09](../specs/C09-dataset-access-control.md)).

### Authentication, sessions, and transport

* **Access levels and principals.** Grants assign `read`, `write`, or `admin` per dataset name or pattern, plus server-wide `metrics`, `federate`, and `server-admin` permissions. Datasets hidden from a caller answer `404`.
* **Identity providers.** Supports HTTP Basic credentials with argon2id password hashes, scoped expiring revocable API tokens (hashed at rest and bounded by their owner's grants), native OIDC sign-in with Authorization Code + PKCE, back-channel logout, optional idle timeouts, JWKS-verified JWT access tokens with key rotation, Cloudflare Access assertions, and trusted forward-auth proxies, with group-to-role mapping. `sparkles auth login` authenticates the CLI via browser loopback or device code.
* **Consistent policy reloads and revocation.** Requests overlapping a policy reload, token reissue, or group change re-evaluate against the updated state, returning `503` (`authority-changed`, `Retry-After: 1`) without consuming `preauth` limits if changes keep overlapping. Stale password verifications cannot repopulate caches or create sessions, and OIDC sign-ins recheck admission after the provider exchange. Creating sessions or tokens leaves other callers' caches and streams untouched, while logout, eviction, expiry, and revocation terminate only the affected credential's active streams (`access-changed`), including on idle timeout.
* **OAuth 2.1 for MCP.** With OIDC and `serve --mcp`, `/$/mcp` publishes RFC 9728 protected-resource metadata, requires tokens whose audience matches `/$/mcp`, and returns `insufficient_scope` challenges for missing `sparkles:read`, `sparkles:update`, or `sparkles:admin` scopes, which can only narrow user grants ([API.md](../API.md#oauth-for-the-mcp-endpoint)).
* **TLS and browser hardening.** `serve --tls-cert --tls-key` serves HTTPS with rustls, HTTP/2 via ALPN, and certificate reload. Cookies follow CSRF and CORS protections.
* **Rate limiting and outbound policy.** Optional per-client GCRA rate limits and concurrency caps apply per request class with per-dataset overrides, answering `429` or `503` with `Retry-After` and `RateLimit` headers and reloading on `SIGHUP`. With auth enabled, a `preauth` stage throttles failed credential checks per IP address and IPv6 `/48` before hashing passwords. Outbound requests from `SERVICE`, `LOAD`, imports, embeddings, model providers, and notifications share one [outbound network policy](../USAGE.md#outbound-requests-service-and-load).
* **Audit logging.** Dataset and branch administration emit `INFO` records under `sparkles::audit` with actor and operation identities, recording clone source commits after registration and bounded failure codes even if an HTTP client disconnects while a worker continues. See the [operations guide](../operations/README.md#observability) for local CLI and scratch-branch expiry audit events.

### Graph and triple protections

* **Graph and endpoint grants.** Grants can restrict a caller to selected named graphs, selected endpoints, or both ([API.md](../API.md#graph-level-access-control), [C12](../specs/C12-graph-access-control.md)). Queries, search, schema reports, diffs, change feeds, and Graph Store requests operate on a filtered view where hidden graphs behave as absent, and writes outside permitted graphs fail before committing.
* **View grants.** A grant's `endpoints` can name an incremental view as `view:{name}`, which declassifies the view for its grantee without opening an endpoint, adding a graph or granting a level. Other callers read a view only when their grants cover every quad or contain the grants of the view's principal, and each refusal or declassified read is audited ([API.md](../API.md#incremental-views), [F13](../specs/F13-incremental-views-and-provenance.md#view-access-by-grant-containment)).
* **Triple protections.** Rules hide triples by predicate, subject class, graph, or a SPARQL pattern bound with the caller's username, roles, and groups, and grants lift protections by name ([API.md](../API.md#protections-of-triples), [C12b](../specs/C12b-triple-access-control.md)). Every read path—including statistics, path search, full-text and vector search, `DESCRIBE`, schema reports, diffs, stored queries, MCP tools, and RDFS on read—enforces visible triples, and writes of protected triples are refused. Callers with unrestricted access keep standard query plans and caches.

### Read-view preparation limits

Rust callers can prepare graph, predicate, and class permission masks under explicit memory, deadline, and cancellation limits using `GraphAccess::masked_controlled` and `Preparation` ([C29](../specs/C29-bounded-view-preparation/README.md)).

* **Memory and concurrency controls.** `Control` propagates cancellation and deadlines through binding, scans, vocabulary decoding, class closure, ordering, and supported guard providers. The memory cap covers temporary peaks and retained policy and mask storage (including delta nodes copied by the mask, while excluding the caller's shared input snapshot and delta so a ~40 MB cap admits a mask over a 100,000-quad delta). Cached masks must fit the cap, `with_no_wait(true)` refuses contested locks, and refused calls return no partial view. Delta vocabulary lookups use single index probes, and cold masks build all seven permutations in parallel.
* **Ownership contract.** `PreparedView` owns its retained-state reservations and exposes non-escaping policy predicates, metadata accessors, and `PreparedView::read_source` rather than a cloneable raw snapshot or `GraphAccess` reference. `BoundEvidenceView` follows the same boundary through `BoundEvidenceView::read_source` and `BoundEvidenceView::prepare_view`, retaining its evidence view, reservation, store instance, and weak policy identity while the prepared view lives. Already-masked snapshots are refused unless the requested policy hides no triples. Base-snapshot deduplication tables hold only non-owning weak references, separate controlled `masked_controlled` masks from legacy `GraphAccess::masked` slots so legacy callers cannot acquire paid controlled masks, and evict dead slots on drop on a best-effort basis with dead-entry pruning on later lookups and insertions so masked delta nodes and visible-graph lists do not outlive their holders without credit. A `GraphAccess` bound via `GraphAccess::bound` carries that view's identity across top-level and nested rules, and checked read paths accept only the matching view while refusing another commit, store, uncommitted view, or masked derivative. Unbound policy builders (`Access::view`, `Grants::graphs`, `guard::apply`) construct reusable definitions outside a query budget before binding, and legacy unbudgeted helpers (`GraphAccess::masked`, `WriteTxn::read_view`) hand out caller-owned snapshots outside a `Preparation`. Preparation caps do not cover subsequent query execution or shared caches.
* **Guard providers.** Providers implement `GuardDecision::exempt_controlled`, which receives a scoped `GuardSnapshot` rather than a cloneable `Arc<Snapshot>` so providers cannot retain physical snapshots past preparation (defaulting to `Unsupported` without running the legacy callback). The built-in GraphRAG disclosure guard supports controlled preparation, reading compatible indexes in place or building a temporary local index while charging only returned exemptions and skipping repeated `(graph, footprint)` rows.
* **Remaining adoption.** Arbitrary protection-pattern planning and unresolved providers nested inside triple rules or protection graph definitions remain unsupported by the bounded helper, while static nested guards work. Evidence resolution uses this helper, whereas guarded `Dataset` queries, C25 exact references, and U02 readers still need adoption (roadmap [item 395](../ROADMAP.md)). Release diagnostics live in the [C29 Outcome](../specs/C29-bounded-view-preparation/SPEC.md#outcome).

## Configuration and usage

| Topic | Reference |
|---|---|
| Hybrid CPU thread placement and outbound network policy | [Threads on hybrid processors](../USAGE.md#threads-on-hybrid-processors), [Outbound requests](../USAGE.md#outbound-requests-service-and-load) |
| Service description and OpenAPI | [Service description](../API.md#service-description), [OpenAPI description](../API.md#openapi-description) |
| GraphQL, stored queries, and resource catalog | [GraphQL](../API.md#graphql), [Stored queries](../API.md#stored-queries), [Resource catalog](../API.md#resource-catalog) |
| Authentication, OAuth for MCP, and graph/triple access control | [Authentication and access control](../API.md#authentication-and-access-control), [OAuth for MCP](../API.md#oauth-for-the-mcp-endpoint), [Graph access control](../API.md#graph-level-access-control), [Triple protections](../API.md#protections-of-triples) |

## Known gaps

Each limit below cites the [central roadmap](../ROADMAP.md) item tracking it.

### Server, access control and operations

* **Metrics and Fuseki parity.** Sparkles has no JVM metrics, assembler payloads cover only single-dataset settings, and direct Graph Store naming is set only at startup via `--gsp-direct-naming` rather than the settings API (roadmap [item 76](../ROADMAP.md)). Missing ARQ functions (`js:`, `afn:context`, `lfn:`, `JSON` query form, `EXISTS { … }` group element, multi-argument `AGG`, no-op `afn:print`/`afn:execTime`, and code-point `afn:collation`) are tracked in roadmap [item 69](../ROADMAP.md).
* **Access control limits.** The change feed refuses callers with data-dependent triple protections, and protections hide triples rather than IRIs referenced inside visible triples (roadmap [item 75](../ROADMAP.md)).
* **GraphQL and stored query limits.** GraphQL lacks mutations, subscriptions, persisted queries, `persistedOnly`, and a web UI page; nested lists are not connections, nested `first` slices responses after generating all field values, parent ids pass as terms instead of engine ids, and blank-node parents expand into `UNION` branches (roadmap [item 37](../ROADMAP.md)). Stored queries lack stored updates, multi-value parameters, and per-user private queries (roadmap [item 39](../ROADMAP.md)).
* **Resource catalog limits.** The catalog does not fetch remote sources; content-addressed storage, recipes, harvesters, load manifests, mirrors, update monitoring, IRI minting, and vocabulary graph bundles remain planned (roadmap [item 458](../ROADMAP.md)). Export and publication routes do not enforce licence gate verdicts (roadmap [item 525](../ROADMAP.md)), Croissant export is absent (roadmap [item 526](../ROADMAP.md)), and catalog access is restricted to server administrators.
* **Hybrid-core scheduling.** Performance-core pinning applies only on Linux kernels exposing `/sys/devices/cpu_core/cpus` and `/sys/devices/cpu_atom/cpus` (Intel hybrid CPUs) and was tuned on one processor; other hybrid CPUs and macOS are uncovered (roadmap [item 576](../ROADMAP.md)).
* **Single-node deployment, backups, and encryption at rest.** Sparkles runs on a single node on local disk without clustering or replicas (roadmap [item 66](../ROADMAP.md)). Live RDF files on local disk are unencrypted (roadmap [item 65](../ROADMAP.md)). Backups operate per dataset without whole-server backups, retained-history capture, or storage-retention policies (roadmap [item 61](../ROADMAP.md)). Backup repository keys cannot be managed through the HTTP API, Vault, or cloud KMS, write-only age-recipient repositories are not implemented, and without `--secrets-key` runtime secrets are sealed by a generated key stored inside the data directory (roadmap [item 65](../ROADMAP.md)).

## Roadmap

Core design specifications for this area are [C09](../specs/C09-dataset-access-control.md), [C12](../specs/C12-graph-access-control.md), [C12b](../specs/C12b-triple-access-control.md), [C29](../specs/C29-bounded-view-preparation/README.md), [C30](../specs/C30-resource-catalog/README.md), [C32](../specs/C32-linked-data-publishing/README.md), [G08](../specs/G08-fuseki-configuration.md), and [X03](../specs/X03-openapi-and-completions.md).

The central [roadmap](../ROADMAP.md) owns priorities and dependencies, and its [completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs) cover items 37, 39, 61, 65, 66, 69, 75, 76, 395, 458, 461, 525, 526, and 576.
