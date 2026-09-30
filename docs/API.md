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
| GET    | `/$/schema/{ds}`             | *Extension.* `SchemaSummary`: classes and predicates with exact counts and their declarations; see [Schema discovery](#schema-discovery). |
| GET    | `/$/schema/{ds}/classes`     | *Extension.* `Page<ClassEntry>` |
| GET    | `/$/schema/{ds}/predicates`  | *Extension.* `Page<PredicateEntry>` |
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

## Schema discovery

`GET /$/schema/{ds}` reports the classes and predicates of a dataset in two separate
layers:

* **observed**: exact counts over the selected graphs at one snapshot. A triple stored in
  several selected graphs counts once. These are measurements of the current data, not
  constraints: `maxPerSubject: 1` only says that no subject has two values *now*.
* **declared**: what the RDFS/OWL vocabulary in the data asserts (`rdf:type` `owl:Class`,
  `rdfs:subClassOf`, `rdfs:domain`, `owl:FunctionalProperty`, labels, …). Only IRI objects
  are listed; blank-node class expressions (`owl:Restriction`, …) are counted in
  `totals.anonymousClassExpressions`.

A class is listed when it is an IRI object of `rdf:type` in the selection, is declared
with `rdf:type rdfs:Class | owl:Class | rdfs:Datatype`, or is an IRI subject or object of
`rdfs:subClassOf`, `owl:equivalentClass` or `owl:disjointWith`. A predicate is listed when
it occurs in the selection (`observed.triples > 0`) or is declared (a property type,
`rdfs:domain`/`range`/`subPropertyOf`, `owl:inverseOf`) with no triples. Nothing is
truncated: every list reports its `total` and is paginated with a cursor.

Parameters (all optional; the read-only server allows them):

| Param | Values | Default | Meaning |
|---|---|---|---|
| `graph` | `default`, `union`, a graph IRI; also `urn:x-arq:DefaultGraph`, `urn:x-arq:UnionGraph` | `default` | Graphs whose triples are counted. `default` is every graph with `--union-default-graph`. A graph IRI with no quads → `404`. |
| `declaredGraph` | same | same as `graph` | Graphs read for declarations (an ontology in its own named graph) |
| `reasoning` | `true`, `false` | `true` if the dataset has materialized inferences | Count `urn:x-sparkles:inferred` as part of `default` / `union` |
| `declared` | `asserted`, `all` | `asserted` | `all` also reads declarations from the inferred graph (which holds the transitive closure of `rdfs:subClassOf`, `rdfs:Resource` supers, …) |
| `limit` | 1–10000 | 1000 | Page size (the summary uses it for both first pages) |
| `cursor` | opaque | — | The `next` of the previous page; send the same selection parameters with it |
| `timeout` | seconds | server query timeout | Budget for computing the report |

```ts
type SchemaSummary = {
  schemaFormat: 1;                 // version of this JSON shape
  dataset: string;
  snapshot: { version: number;     // changes on every commit and compaction; restarts with the server
              generation: string;  // base index generation
              computedAt: string };// RFC 3339
  selection: { graph: string; declaredGraph: string; reasoning: boolean; declared: "asserted" | "all" };
  totals: { triples: number;       // distinct triples in the selection
            classes: number; predicates: number;
            anonymousTypeTargets: number;        // blank-node objects of rdf:type
            anonymousClassExpressions: number }; // blank-node objects of class axioms, rdfs:domain, rdfs:range
  ontology: { iri: string; labels: Lit[]; versionInfo: Lit[]; comments: Lit[] }[];  // every owl:Ontology
  hierarchy: { roots: string[];    // classes with no declared superclass outside their own subClassOf cycle
                                   // (one per cycle), most subclasses first, then by IRI
               cycles: string[][] };// subClassOf cycles with more than one member (A ⊑ A is not a cycle)
  classes: Page<ClassEntry>;
  predicates: Page<PredicateEntry>;
};
type Page<T> = { items: T[]; total: number; next: string | null };  // items in IRI order
type Lit = { value: string; lang?: string };

type ClassEntry = {
  iri: string;
  builtin: boolean;                // rdf:, rdfs:, owl:, xsd: or sh: namespace
  observed: { instances: number }; // distinct subjects with rdf:type C (no subclass roll-up)
  declared: { types: string[];     // subset of rdfs:Class, owl:Class, rdfs:Datatype
              superClasses: string[]; equivalentClasses: string[]; disjointWith: string[];
              labels: Lit[]; comments: Lit[] };
};
type PredicateEntry = {
  iri: string;
  builtin: boolean;
  observed: {
    triples: number; distinctSubjects: number; distinctObjects: number;
    maxPerSubject: number;         // largest number of distinct objects of one subject, in this snapshot
    subjectsWithMultiple: number;  // subjects with two or more distinct objects
    objects: {
      iri?: KindCount; blank?: KindCount; tripleTerm?: KindCount;
      literals: { datatype: string;  // xsd:string for simple literals; rdf:langString / rdf:dirLangString
                  triples: number; distinct: number;  // distinct terms: "01"^^xsd:integer ≠ "1"^^xsd:integer
                  languages?: { lang: string; direction?: "ltr" | "rtl"; triples: number }[] }[];
    };
  };
  declared: { types: string[];     // rdf:Property, owl:ObjectProperty, owl:FunctionalProperty, …
              domains: string[]; ranges: string[]; superProperties: string[]; inverseOf: string[];
              labels: Lit[]; comments: Lit[] };
};
type KindCount = { triples: number; distinct: number };
```

**Pagination.** Every page of a listing comes from the report of one snapshot. The
server keeps the last report per dataset; the summary and a page request without a
cursor reuse it while the snapshot and the selection are unchanged, and compute a new one
otherwise. A cursor from an older snapshot is still served while that report is the one
kept; once a newer report replaces it the request fails with `409` and the client
restarts from the first page. Cursors do not survive a restart.

**Errors** (`{ "error" }` body): `400` for a bad parameter, a malformed cursor or a
cursor issued for other selection parameters; `404` for an unknown dataset or a graph
with no quads; `408` when the report did not finish within `timeout`
(`"schema discovery exceeded 60s while scanning predicates (412/9031); narrow graph= or
raise timeout="`); `409` as above; `413` when there are more than `--schema-max-entries`
(default 1,000,000) classes or predicates (`"dataset has 1204331 classes (limit
1000000)"`). A report is never returned partially.

The counts come from one ordered pass over the PSO and one over the POS index per
predicate, so a report costs about two sequential reads of the selected triples.

The CLI equivalent prints the complete report without pagination:
`sparkles schema --loc DB [--graph default|union|IRI] [--declared-graph G]
[--no-inferences] [--declared asserted|all] [--format text|json] [--timeout S]
[--max-entries N]` (or `--data FILE…`). `json` is the `SchemaSummary` with every item and
`next: null`; `text` prints one line per class and per predicate. It exits with status 2
when the timeout or the entry cap is exceeded. In Rust, `sparkles::schema::discover`.

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
with `400` for parse errors, `404` unknown dataset, `408` timeout, `409` conflict, `500` otherwise.
