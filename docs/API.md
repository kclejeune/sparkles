# Sparkles HTTP API

The server speaks the **Fuseki** protocol surface (so existing Jena tooling —
`rdfconnection`, `s-query`, YASGUI, etc. — works unchanged) plus a small set of
`/$/…` extensions used by the web UI.

All admin endpoints live under `/$/`. Dataset names match `[A-Za-z0-9_.-]+` and are
addressed as `/{ds}`. JSON responses use `application/json`.

## Server

| Method | Path          | Description |
|--------|---------------|-------------|
| GET    | `/$/ping`     | Plain-text timestamp. Liveness check. |
| GET    | `/$/server`   | `{ "version", "startedAt", "uptimeSeconds", "datasets": [DatasetInfo] }` |

## Datasets (admin)

| Method | Path                         | Description |
|--------|------------------------------|-------------|
| GET    | `/$/datasets`                | `{ "datasets": [DatasetInfo] }` |
| POST   | `/$/datasets`                | Create. Form or JSON body: `dbName`, `dbType` = `persistent` \| `mem`. `201` on success, `409` if exists. |
| GET    | `/$/datasets/{ds}`           | `DatasetInfo` |
| DELETE | `/$/datasets/{ds}`           | Remove dataset (and its files). |
| GET    | `/$/stats/{ds}`              | `DatasetStats` |
| POST   | `/$/compact/{ds}`            | Merge delta (updates) into a freshly built, sorted base index. Returns `Task`. |
| POST   | `/$/backup/{ds}`             | Write gzipped N-Quads dump to `<data>/backups/`. Returns `Task`. |
| POST   | `/$/reason/{ds}`             | Materialize inferences. JSON body `{ "profile": "rdfs" \| "owl-rl" \| "rules", "rules"?: string }`. Returns `Task`. |
| DELETE | `/$/reason/{ds}`             | Drop materialized inferences. |
| GET    | `/$/tasks`                   | `[Task]` |
| GET    | `/$/tasks/{id}`              | `Task` |
| POST   | `/$/cache/clear/{ds}`        | *Extension (no Fuseki equivalent).* Drop the dataset's cached query results. `{ "cleared": number /* entries */, "bytes": number }` |
| GET    | `/$/prefixes/{ds}`           | `{ "prefixes": { "rdf": "http://…#", … } }` — prefixes seen during loading plus well-known ones. |

```ts
type DatasetInfo = {
  name: string;            // "ds"
  type: "persistent" | "mem";
  endpoints: { query: string; update: string; gsp: string; upload: string; shacl?: string /* absent when built without the `shacl` feature */ };
  quads: number;           // approximate total (base + delta)
  reasoning: null | { profile: string; inferred: number; at: string };
};

type DatasetStats = {
  name: string;
  quads: number;           // base + inserts − deletes
  baseQuads: number;
  deltaInserts: number;
  deltaDeletes: number;
  terms: number;           // vocabulary size
  graphs: { name: string | null; quads: number }[];   // null = default graph
  predicates: { iri: string; count: number; distinctSubjects: number; distinctObjects: number }[]; // top 100
  classes: { iri: string; instances: number }[];      // top 100 by rdf:type
  diskBytes: number;
  cache: { entries: number; bytes: number; hits: number; misses: number };        // decoded-block cache (--cache-mb)
  resultCache: { enabled: boolean; entries: number; bytes: number; hits: number; misses: number }; // query (sub)result cache (--result-cache-mb)
};

type Task = {
  id: string; kind: "compact" | "backup" | "reason" | "load";
  dataset: string; state: "running" | "done" | "failed";
  startedAt: string; finishedAt?: string; message?: string; progress?: number /*0..1*/;
};
```

## Per-dataset SPARQL protocol (Fuseki compatible)

| Method     | Path                  | Description |
|------------|-----------------------|-------------|
| GET/POST   | `/{ds}` , `/{ds}/sparql`, `/{ds}/query` | SPARQL 1.1 Query protocol (`query=` param, `application/sparql-query` body, or form). `default-graph-uri` / `named-graph-uri` supported. |
| POST       | `/{ds}/update`        | SPARQL 1.1 Update protocol (`update=` form or `application/sparql-update` body). |
| GET/PUT/POST/DELETE/HEAD | `/{ds}/data` , `/{ds}/get` | Graph Store Protocol. `?default` or `?graph=<iri>`; no param on GET = whole dataset as N-Quads/TriG. |
| POST       | `/{ds}/upload`        | Multipart file upload; format chosen from filename extension / content-type. Optional `graph` field. |
| POST       | `/{ds}/shacl`         | SHACL validation (Fuseki `/{ds}/shacl`); see [SHACL validation](#shacl-validation). |

Content negotiation via `Accept` or the `format=` parameter (Fuseki style):

* SELECT/ASK: `application/sparql-results+json` (default), `application/sparql-results+xml`,
  `text/csv`, `text/tab-separated-values`, and `application/x-sparkles+json` (see below).
* CONSTRUCT/DESCRIBE/GSP GET: `text/turtle` (default), `application/n-triples`,
  `application/n-quads`, `application/trig`, `application/ld+json`, `application/rdf+xml`.

Query parameters beyond the standard protocol:

* `timeout=<seconds>` — query timeout (default 60 s).
* `send=<n>` — cap on rows serialized (the UI uses this so a huge result does not hang the browser; `meta.totalRows` still reports the full count).
* `reasoning=true|false` — include materialized inferences (default `true` if present).
* `nocache=true` — bypass the query result cache: nothing is read from or stored in it
  (for benchmarking; `explain` accepts it too). The server-wide budget is set with
  `sparkles serve --result-cache-mb N` (default 512, `0` disables the cache); the cache is
  keyed by snapshot version, so updates invalidate it, and `POST /$/cache/clear/{ds}`
  empties it.

## Commits

Every dataset has a **dataset id** (a UUID created with it) and a gap-free **commit
sequence**. Each write that changes data (update, Graph Store PUT/POST/DELETE, upload,
load, reasoning) gets the next `seq`. A write with no net effect, such as inserting a
quad that is already present, creates no commit. Commit 0 is the root. Compaction keeps
the head. Ids survive restarts and are durable exactly when the data is.

**Headers.** Every successful query, update, Graph Store and explain response carries:

```
Sparkles-Commit: 42                 (the commit a read saw, or a write produced)
Sparkles-Dataset-Id: 3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa
```

Both are exposed to cross-origin clients. The `application/x-sparkles+json` result
format also has `meta.commit` and `meta.datasetId`.

**Receipts.** Write responses keep their Fuseki-compatible bodies by default. With
`Accept: application/x-sparkles+json` or `receipt=true`, the body (same status; `200`
instead of `204` for Graph Store DELETE) adds:

```ts
type Receipt = {
  dataset: string; datasetId: string;
  committed: boolean;            // false: no net change, `commit` is the unchanged head
  commit: Commit;
};
type Commit = {
  seq: number; parent: number | null; ref: string;   // "commit:42"
  timestamp: string;             // RFC 3339 UTC with milliseconds, never decreasing
  kind: "create" | "baseline" | "update" | "gsp-put" | "gsp-post" | "gsp-delete"
      | "upload" | "load" | "reason" | "reason-clear" | "transaction" | "unknown";
  inserted: number; deleted: number;   // net change relative to the parent
  quads: number;                        // dataset size after the commit
  generation: string;                   // index generation it was made in
  bulk: boolean;                        // made by rebuilding the index
  exact: boolean;                       // false: a bulk commit that also deleted
};
```

| Method | Path | Description |
|--------|------|-------------|
| GET | `/$/commits/{ds}` | Newest commits first: `?limit=` (default 50, max 1000), `?before=<seq>` pages backwards, `?after=<seq>` lists oldest first after `seq`. Returns `{dataset, datasetId, head, firstRetained, complete, commits: Commit[], next: string \| null}`. |
| GET | `/$/commits/{ds}/{ref}` | One commit; `ref` is `42`, `commit:42` or `head`. `404` beyond the head, `410` if no longer retained. |

`GET /$/datasets[/{ds}]` entries gain `id`, `head` and `modified` (the head's timestamp).
`sparkles log --loc DB [--limit N] [--before SEQ | --after SEQ | --at REF] [--format json]`
lists commits without taking the database lock, so it works next to a running server.

## SHACL validation

`POST /{ds}/shacl?graph=default|union|<iri>` validates a data graph of the dataset against
the shapes graph in the request body (Fuseki semantics):

* **Body**: the shapes graph. `Content-Type` selects the syntax: `application/n-triples`,
  `application/rdf+xml`, `application/ld+json`, `application/trig`, `application/n-quads`
  (all graphs of a quad format are merged); Turtle for `text/turtle` and for any other or
  absent content type (e.g. curl's default `application/x-www-form-urlencoded`).
* **`graph`**: `default` (the default; the dataset's default graph, i.e. the union of all
  graphs with `--union-default-graph`), `union` (all graphs, `urn:x-arq:UnionGraph`), or a
  graph IRI (`404` if the graph does not exist). The Jena special IRIs
  `urn:x-arq:DefaultGraph` / `urn:x-arq:UnionGraph` are accepted as well.
* **`reasoning=true|false`**: when the dataset has materialized inferences, validation runs
  over data ∪ `urn:x-sparkles:inferred` unless `reasoning=false`; with `false` the inferred
  graph is also left out of `graph=union`.
* **`timeout=<seconds>`**: as for queries (server default otherwise); `408` on timeout.
* Supports SHACL Core and SHACL-SPARQL. Parse errors in the shapes graph → `400`.

The response is the validation report (`200` whether or not the data conforms),
negotiated via `Accept` or `format=`:

| Accept / `format=` | Body |
|---|---|
| `text/turtle` / `ttl` (default) | `sh:ValidationReport` in Turtle |
| `application/n-triples` / `nt`, `application/ld+json` / `jsonld`, `application/rdf+xml` / `rdfxml` | the same report triples |
| `application/json` / `json` | compact JSON (below) |
| `format=text` | human-readable summary (one line per result) |

```ts
type ShaclReport = {
  conforms: boolean;
  results: {
    focusNode: Term;
    resultPath: Term | { type: "path"; value: string /* SPARQL property path */ } | null;
    value: Term | null;
    sourceShape: Term;
    sourceConstraintComponent: Term;   // e.g. { type: "uri", value: "http://www.w3.org/ns/shacl#MinCountConstraintComponent" }
    sourceConstraint?: Term;           // SHACL-SPARQL constraints
    severity: Term;                    // sh:Violation | sh:Warning | sh:Info
    messages: string[];                // sh:resultMessage texts
  }[];
};
```

The CLI equivalent is `sparkles shacl --loc DB --shapes shapes.ttl [--graph default|union|IRI]
[--format ttl|json|text|nt|jsonld|rdfxml] [--no-inferences]` (or `--data FILE…` to validate
files in memory); like Jena's `shacl validate` it exits with status 1 when the data does not
conform.

## `application/x-sparkles+json` (UI result format)

Rich result format inspired by QLever's `qlever-json`, used by the UI for results
rendering and query-plan visualization:

```ts
type SparklesResult = {
  queryType: "SELECT" | "ASK" | "CONSTRUCT" | "DESCRIBE";
  vars?: string[];                        // SELECT
  rows?: (Term | null)[][];               // SELECT; null = unbound
  boolean?: boolean;                      // ASK
  triples?: [Term, Term, Term][];         // CONSTRUCT / DESCRIBE
  meta: {
    totalRows: number; sentRows: number;
    timing: { parseMs: number; planMs: number; execMs: number; serializeMs: number; totalMs: number };
    plan: PlanNode;                        // executed operator tree
  };
};
type Term =
  | { type: "uri"; value: string }
  | { type: "bnode"; value: string }
  | { type: "literal"; value: string; datatype?: string; "xml:lang"?: string }
  | { type: "triple"; value: { subject: Term; predicate: Term; object: Term } };
type PlanNode = {
  operator: string;          // "IndexScan", "Join", "Sort", "Filter", ...
  description: string;       // human readable, e.g. "PSO ?s <p> ?o"
  columns: string[];         // variables produced
  sortedOn: string[];
  estimatedRows: number; estimatedCost: number;
  actualRows: number; timeMs: number;       // wall time incl. children
  cached: boolean;
  children: PlanNode[];
};
```

## Explain

`GET|POST /{ds}/explain?query=…` → `{ "algebra": string /* SSE */, "plan": PlanNode }` (plan not executed; `actualRows`=-1).

## Errors

Non-2xx responses carry `{ "error": string, "detail"?: string, "line"?: number, "column"?: number }`
with `400` for parse errors, `404` unknown dataset, `408` timeout, `409` conflict, `503` when
a write-ahead log write failed (writes are refused until restart; reads continue), `500`
otherwise.
