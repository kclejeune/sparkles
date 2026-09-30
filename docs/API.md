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
| GET    | `/$/prefixes/{ds}`           | `{ "prefixes": { "rdf": "http://…#", … } }` — prefixes seen during loading plus well-known ones. |

```ts
type DatasetInfo = {
  name: string;            // "ds"
  type: "persistent" | "mem";
  endpoints: { query: string; update: string; gsp: string; upload: string };
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
  cache: { entries: number; bytes: number; hits: number; misses: number };
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

Content negotiation via `Accept` or the `format=` parameter (Fuseki style):

* SELECT/ASK: `application/sparql-results+json` (default), `application/sparql-results+xml`,
  `text/csv`, `text/tab-separated-values`, and `application/x-sparkles+json` (see below).
* CONSTRUCT/DESCRIBE/GSP GET: `text/turtle` (default), `application/n-triples`,
  `application/n-quads`, `application/trig`, `application/ld+json`, `application/rdf+xml`.

Query parameters beyond the standard protocol:

* `timeout=<seconds>` — query timeout (default 60 s).
* `send=<n>` — cap on rows serialized (the UI uses this so a huge result does not hang the browser; `meta.totalRows` still reports the full count).
* `reasoning=true|false` — include materialized inferences (default `true` if present).

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
with `400` for parse errors, `404` unknown dataset, `408` timeout, `409` conflict, `500` otherwise.
