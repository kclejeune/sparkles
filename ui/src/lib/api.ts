// Typed client for the Sparkles HTTP API (see docs/API.md — the source of truth).
// All paths are absolute from the server root; the UI itself lives under /ui/.

import { CSRF_HEADER, needsCsrf, type Level } from './auth';
import { fmtBytes, fmtInt } from './format';

export type DatasetType = 'persistent' | 'mem';

export type DatasetInfo = {
  name: string;
  type: DatasetType;
  endpoints: { query: string; update: string; gsp: string; upload: string; shacl?: string };
  quads: number;
  reasoning: null | {
    profile: string;
    inferred: number;
    at: string;
    /** Commit the inferences were materialized at (null: unknown). */
    commit?: number | null;
    /** null: freshness unknown (legacy status). */
    stale?: boolean | null;
    commitsSince?: number | null;
  };
  /** Clones: the source dataset id and commit this dataset was copied from. */
  forkedFrom?: { id: string; seq: number };
  /** Clones: the source and when the copy was made (`origin.json`). */
  origin?: DatasetOrigin;
  /** Dataset id (a UUID created with the dataset); absent on servers that predate commits. */
  id?: string;
  /** Head commit sequence number. */
  head?: number;
  /** Timestamp of the head commit. */
  modified?: string;
  /** Full-text index summary (null: disabled; absent: server without the field). */
  text?: { state: TextState; docs: number } | null;
  /** The caller's level on the dataset; absent without auth (then everything goes). */
  access?: Level;
};

/** Per-request budgets of the server; 0 means unlimited. */
export type Limits = {
  timeoutSeconds: number;
  /** Absent on servers that predate it. */
  updateTimeoutSeconds?: number;
  queryMemoryBytes: number;
  maxResultBytes: number;
  maxRows: number;
};

export type ServerInfo = {
  version: string;
  startedAt: string;
  uptimeSeconds: number;
  datasets: DatasetInfo[];
  /** Absent on servers that predate budgets. */
  limits?: Limits;
  /** Absent on servers that predate it. */
  readOnly?: boolean;
};

/** `GET /$/ready` (the same document with status 503 when not ready). */
export type ReadyInfo = {
  status: 'starting' | 'ready' | 'draining' | 'degraded';
  ready: boolean;
  uptimeSeconds: number;
  datasets: {
    name: string;
    type: DatasetType;
    state: 'open' | 'opening' | 'failed';
    ready: boolean;
    generation?: string;
    walBytes: number;
    deltaQuads: number;
    error?: string;
  }[];
};

export type Operation =
  | 'query'
  | 'update'
  | 'gsp'
  | 'upload'
  | 'shacl'
  | 'explain'
  | 'admin'
  | 'other';
export type Outcome =
  | 'ok'
  | 'client_error'
  | 'error'
  | 'timeout'
  | 'cancelled'
  | 'budget'
  | 'rate_limited'
  | 'denied'
  | 'rejected';
export type LimitClass = 'auth' | 'query' | 'update' | 'admin';
export type BudgetKind = 'rows' | 'memory' | 'result-bytes';

type CacheStats = {
  bytes: number;
  capacityBytes: number;
  entries: number;
  hits: number;
  misses: number;
};

/** `GET /$/metrics?format=json`: the metrics registry as JSON. */
export type MetricsSnapshot = {
  formatVersion: 1;
  version: string;
  uptimeSeconds: number;
  ready: boolean;
  processResidentBytes: number | null;
  limits: Limits;
  /** Histogram upper bounds in seconds, without +Inf. */
  bucketBounds: number[];
  /** Requests in progress per operation. */
  active: Record<Operation, number>;
  requests: RequestSeries[];
  datasets: {
    name: string;
    quads: number;
    deltaInserts: number;
    deltaDeletes: number;
    walBytes: number;
    diskBytes: number;
    resultRows: number;
    budgetExceeded: Record<BudgetKind, number> | null;
    /** Requests refused by a rate or concurrency limit, per limit class. */
    rateLimited?: Record<LimitClass, number> | null;
    blockCache: CacheStats;
    resultCache: CacheStats & { enabled: boolean };
  }[];
};

/** Counters of one (dataset, operation) pair. */
export type RequestSeries = {
  /** A dataset name, `$none` (no existing dataset) or `$other` (beyond the label cap). */
  dataset: string;
  operation: Operation;
  outcomes: Record<Outcome, number>;
  count: number;
  sumSeconds: number;
  /** Cumulative counts per histogram bucket; the last (+Inf) equals `count`. */
  buckets: number[];
  responseBytes: number;
};

export type DatasetStats = {
  name: string;
  quads: number;
  baseQuads: number;
  deltaInserts: number;
  deltaDeletes: number;
  terms: number;
  graphs: { name: string | null; quads: number }[];
  predicates: { iri: string; count: number; distinctSubjects: number; distinctObjects: number }[];
  classes: { iri: string; instances: number }[];
  diskBytes: number;
  /** Decoded-block cache. */
  cache: { entries: number; bytes: number; hits: number; misses: number };
  /** Query (sub)result cache; absent on servers that predate it. */
  resultCache?: { enabled: boolean; entries: number; bytes: number; hits: number; misses: number };
  /** Reasoning status; absent on servers that predate it. */
  reasoning?: ReasoningStatus | null;
};

export type TaskKind = 'compact' | 'backup' | 'reason' | 'load' | 'clone' | 'text-rebuild';
export type Task = {
  id: string;
  kind: TaskKind;
  dataset: string;
  /** The dataset a task creates (clone). */
  target?: string;
  state: 'running' | 'done' | 'failed';
  startedAt: string;
  finishedAt?: string;
  message?: string;
  progress?: number;
};

export type Term =
  | { type: 'uri'; value: string }
  | { type: 'bnode'; value: string }
  | { type: 'literal'; value: string; datatype?: string; 'xml:lang'?: string }
  | { type: 'triple'; value: { subject: Term; predicate: Term; object: Term } };

export type PlanNode = {
  operator: string;
  description: string;
  columns: string[];
  sortedOn: string[];
  estimatedRows: number;
  estimatedCost: number;
  actualRows: number;
  timeMs: number;
  cached: boolean;
  children: PlanNode[];
};

export type Timing = {
  parseMs: number;
  planMs: number;
  execMs: number;
  serializeMs: number;
  totalMs: number;
};

export type QueryType = 'SELECT' | 'ASK' | 'CONSTRUCT' | 'DESCRIBE';

export type SparklesResult = {
  queryType: QueryType;
  vars?: string[];
  rows?: (Term | null)[][];
  boolean?: boolean;
  triples?: [Term, Term, Term][];
  meta: {
    totalRows: number;
    sentRows: number;
    timing: Timing;
    plan: PlanNode;
    /** Peak estimated memory of intermediate results; absent on older servers. */
    memory?: { peakBytes: number };
    /** The commit the query read (absent on servers that predate commits). */
    commit?: number;
    datasetId?: string;
  };
  /** From the `Sparkles-Inferences` header: the result used outdated inferences. */
  inferences?: InferencesNotice;
};

export type ExplainResult = { algebra: string; plan: PlanNode };

export type ReasonProfile = 'rdfs' | 'owl-rl' | 'rules';

/** A budget a request exceeded (the body of a `507`). */
export type Budget = { kind: BudgetKind; limit: number; requested: number };

type ApiErrorExtra = {
  detail?: string;
  line?: number;
  column?: number;
  requestId?: string;
  budget?: Budget;
};

/** Error shape for non-2xx responses: `{ error, detail?, line?, column? }`. */
export class ApiError extends Error {
  status: number;
  detail?: string;
  line?: number;
  column?: number;
  /** The server's `X-Request-Id` for this request, to find it in the server log. */
  requestId?: string;
  budget?: Budget;
  constructor(status: number, message: string, extra: ApiErrorExtra = {}) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.detail = extra.detail;
    this.line = extra.line;
    this.column = extra.column;
    this.requestId = extra.requestId;
    this.budget = extra.budget;
  }
}

function budgetOf(body: Record<string, unknown>): Budget | undefined {
  const kind = body.budget;
  if (kind !== 'rows' && kind !== 'memory' && kind !== 'result-bytes') return undefined;
  return { kind, limit: Number(body.limit ?? 0), requested: Number(body.requested ?? 0) };
}

/** What to do about an exceeded budget, in plain words. */
export function budgetHint(b: Budget): string {
  switch (b.kind) {
    case 'result-bytes':
      return `Result too large (limit ${fmtBytes(b.limit)}). Add a LIMIT or narrow the query.`;
    case 'memory':
      return `The query needs too much memory (limit ${fmtBytes(b.limit)}). Narrow the query or make its patterns more selective.`;
    case 'rows':
      return `An intermediate result is too large (limit ${fmtInt(b.limit)} rows). Narrow the query.`;
  }
}

export const SPARKLES_JSON = 'application/x-sparkles+json';

const enc = encodeURIComponent;

async function toError(res: Response): Promise<ApiError> {
  const requestId = res.headers.get('X-Request-Id') ?? undefined;
  let text = '';
  try {
    text = await res.text();
  } catch {
    /* ignore */
  }
  try {
    const body = JSON.parse(text);
    if (body && typeof body.error === 'string') {
      return new ApiError(res.status, body.error, {
        detail: body.detail,
        line: body.line,
        column: body.column,
        requestId,
        budget: budgetOf(body),
      });
    }
  } catch {
    /* not JSON */
  }
  const fallback = text.trim().slice(0, 500) || res.statusText || 'Request failed';
  return new ApiError(res.status, `${res.status} ${fallback}`, { requestId });
}

/** How requests authenticate: the CSRF token of the session, and what a 401 does. */
const authHooks: { csrf: () => string | undefined; unauthorized: () => void } = {
  csrf: () => undefined,
  unauthorized: () => {},
};

export function setAuthHooks(h: Partial<typeof authHooks>) {
  Object.assign(authHooks, h);
}

/** `init` with the CSRF header added to unsafe requests of a session. */
function withCsrf(init: RequestInit): RequestInit {
  const token = authHooks.csrf();
  if (!token || !needsCsrf(init.method)) return init;
  const headers = new Headers(init.headers);
  headers.set(CSRF_HEADER, token);
  return { ...init, headers };
}

export async function request(path: string, init: RequestInit = {}): Promise<Response> {
  let res: Response;
  try {
    res = await fetch(path, withCsrf(init));
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'Cannot reach the Sparkles server', { detail: String(e) });
  }
  if (res.status === 401) authHooks.unauthorized();
  if (!res.ok) throw await toError(res);
  return res;
}

export async function json<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await request(path, {
    ...init,
    headers: { Accept: 'application/json', ...(init.headers ?? {}) },
  });
  const text = await res.text();
  if (!text) return undefined as T;
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(res.status, 'Server returned invalid JSON', { detail: text.slice(0, 300) });
  }
}

const jsonBody = (body: unknown): RequestInit => ({
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

// --- server ------------------------------------------------------------------

export async function ping(signal?: AbortSignal): Promise<string> {
  const res = await request('/$/ping', { signal, cache: 'no-store' });
  return res.text();
}

export const serverInfo = () => json<ServerInfo>('/$/server');

/** Readiness; resolves with the document for `503` (not ready) as well. */
export async function ready(signal?: AbortSignal): Promise<ReadyInfo> {
  let res: Response;
  try {
    res = await fetch('/$/ready', { signal, cache: 'no-store' });
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'Cannot reach the Sparkles server', { detail: String(e) });
  }
  if (res.status !== 200 && res.status !== 503) throw await toError(res);
  return (await res.json()) as ReadyInfo;
}

/** The metrics registry as JSON (`404` when the server runs with `--no-metrics`). */
export const metricsSnapshot = (signal?: AbortSignal) =>
  json<MetricsSnapshot>('/$/metrics?format=json', { signal, cache: 'no-store' });

// --- datasets -----------------------------------------------------------------

export async function listDatasets(): Promise<DatasetInfo[]> {
  const body = await json<{ datasets: DatasetInfo[] }>('/$/datasets');
  return body?.datasets ?? [];
}

export const getDataset = (ds: string) => json<DatasetInfo>(`/$/datasets/${enc(ds)}`);

export const createDataset = (dbName: string, dbType: DatasetType) =>
  json<unknown>('/$/datasets', jsonBody({ dbName, dbType }));

export const deleteDataset = (ds: string) =>
  json<unknown>(`/$/datasets/${enc(ds)}`, { method: 'DELETE' });

export const datasetStats = (ds: string) => json<DatasetStats>(`/$/stats/${enc(ds)}`);

export const compact = (ds: string) => json<Task>(`/$/compact/${enc(ds)}`, { method: 'POST' });

export const backup = (ds: string) => json<Task>(`/$/backup/${enc(ds)}`, { method: 'POST' });

export const reason = (ds: string, profile: ReasonProfile, rules?: string) =>
  json<Task>(
    `/$/reason/${enc(ds)}`,
    jsonBody(profile === 'rules' ? { profile, rules } : { profile }),
  );

/** Drop the dataset's cached query results (`POST /$/cache/clear/{ds}`, a Sparkles extension). */
export const clearResultCache = (ds: string) =>
  json<{ cleared: number; bytes: number }>(`/$/cache/clear/${enc(ds)}`, { method: 'POST' });

export const dropInferences = (ds: string) =>
  json<unknown>(`/$/reason/${enc(ds)}`, { method: 'DELETE' });

export async function listTasks(): Promise<Task[]> {
  const body = await json<Task[] | { tasks: Task[] }>('/$/tasks');
  return Array.isArray(body) ? body : (body?.tasks ?? []);
}

export const getTask = (id: string) => json<Task>(`/$/tasks/${enc(id)}`);

export async function prefixes(ds: string): Promise<Record<string, string>> {
  const body = await json<{ prefixes: Record<string, string> }>(`/$/prefixes/${enc(ds)}`);
  return body?.prefixes ?? {};
}

// --- schema discovery ---------------------------------------------------------

/** A plain or language-tagged literal (labels, comments). */
export type Lit = { value: string; lang?: string };
export type KindCount = { triples: number; distinct: number };

export type SchemaClass = {
  iri: string;
  /** rdf:, rdfs:, owl:, xsd: or sh: namespace */
  builtin: boolean;
  /** Distinct subjects typed with the class in the selected graphs. */
  observed: { instances: number };
  declared: {
    types: string[];
    superClasses: string[];
    equivalentClasses: string[];
    disjointWith: string[];
    labels: Lit[];
    comments: Lit[];
  };
};

export type LiteralGroup = {
  datatype: string;
  triples: number;
  distinct: number;
  languages?: { lang: string; direction?: 'ltr' | 'rtl'; triples: number }[];
};

export type SchemaPredicate = {
  iri: string;
  builtin: boolean;
  /** Exact counts at the report's snapshot: measurements, not constraints. */
  observed: {
    triples: number;
    distinctSubjects: number;
    distinctObjects: number;
    maxPerSubject: number;
    subjectsWithMultiple: number;
    objects: {
      iri?: KindCount;
      blank?: KindCount;
      tripleTerm?: KindCount;
      literals: LiteralGroup[];
    };
  };
  declared: {
    types: string[];
    domains: string[];
    ranges: string[];
    superProperties: string[];
    inverseOf: string[];
    labels: Lit[];
    comments: Lit[];
  };
};

export type Page<T> = { items: T[]; total: number; next: string | null };

export type SchemaSummary = {
  schemaFormat: 1;
  dataset: string;
  snapshot: { version: number; generation: string; computedAt: string };
  selection: {
    graph: string;
    declaredGraph: string;
    reasoning: boolean;
    declared: 'asserted' | 'all';
  };
  totals: {
    triples: number;
    classes: number;
    predicates: number;
    anonymousTypeTargets: number;
    anonymousClassExpressions: number;
  };
  ontology: { iri: string; labels: Lit[]; versionInfo: Lit[]; comments: Lit[] }[];
  hierarchy: { roots: string[]; cycles: string[][] };
  classes: Page<SchemaClass>;
  predicates: Page<SchemaPredicate>;
};

export type SchemaOptions = {
  /** `default`, `union` or a graph IRI. */
  graph?: string;
  /** Graph read for declarations (server default: the same as `graph`). */
  declaredGraph?: string;
  /** Count materialized inferences (server default: yes, when present). */
  reasoning?: boolean;
  declared?: 'asserted' | 'all';
  /** Page size (1–10000). */
  limit?: number;
  timeout?: number;
  signal?: AbortSignal;
};

function schemaParams(opts: SchemaOptions, cursor?: string): string {
  const p = new URLSearchParams();
  if (opts.graph) p.set('graph', opts.graph);
  if (opts.declaredGraph) p.set('declaredGraph', opts.declaredGraph);
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  if (opts.declared) p.set('declared', opts.declared);
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.timeout != null) p.set('timeout', String(opts.timeout));
  if (cursor) p.set('cursor', cursor);
  const s = p.toString();
  return s ? `?${s}` : '';
}

/** `GET /$/schema/{ds}`: the report with the first page of classes and predicates. */
export const schemaSummary = (ds: string, opts: SchemaOptions = {}) =>
  json<SchemaSummary>(`/$/schema/${enc(ds)}${schemaParams(opts)}`, { signal: opts.signal });

/** One page of `GET /$/schema/{ds}/classes|predicates` after `cursor`. */
export function schemaPage<K extends 'classes' | 'predicates'>(
  ds: string,
  list: K,
  cursor: string,
  opts: SchemaOptions = {},
): Promise<Page<K extends 'classes' ? SchemaClass : SchemaPredicate>> {
  return json(`/$/schema/${enc(ds)}/${list}${schemaParams(opts, cursor)}`, {
    signal: opts.signal,
  });
}

/**
 * The complete schema report: the summary with every page of both lists appended
 * (`next` is null in the result). All pages come from the snapshot of the first one;
 * when the server answers 409 (the snapshot changed and its report is gone) the listing
 * restarts from the first page, at most `attempts` times in all.
 */
export async function schema(
  ds: string,
  opts: SchemaOptions = {},
  attempts = 3,
): Promise<SchemaSummary> {
  for (let attempt = 1; ; attempt++) {
    try {
      const summary = await schemaSummary(ds, opts);
      for (const list of ['classes', 'predicates'] as const) {
        const page = summary[list] as Page<SchemaClass | SchemaPredicate>;
        while (page.next) {
          const more = await schemaPage(ds, list, page.next, opts);
          page.items.push(...more.items);
          page.next = more.next;
        }
      }
      return summary;
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409) || attempt >= attempts) throw e;
    }
  }
}

// --- SPARQL -------------------------------------------------------------------

export type QueryOptions = {
  send?: number;
  timeout?: number;
  reasoning?: boolean;
  /** Bypass the server's result cache. */
  nocache?: boolean;
  signal?: AbortSignal;
};

function queryParams(opts: QueryOptions): string {
  const p = new URLSearchParams();
  if (opts.nocache) p.set('nocache', 'true');
  if (opts.send != null) p.set('send', String(opts.send));
  if (opts.timeout != null) p.set('timeout', String(opts.timeout));
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  const s = p.toString();
  return s ? `?${s}` : '';
}

/** Run a query and get the rich UI result format (rows + timing + plan). */
export async function query(
  ds: string,
  sparql: string,
  opts: QueryOptions = {},
): Promise<SparklesResult> {
  const res = await request(`/${enc(ds)}/sparql${queryParams(opts)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/sparql-query', Accept: SPARKLES_JSON },
    body: sparql,
    signal: opts.signal,
  });
  const body = (await res.json()) as SparklesResult;
  const inferences = parseInferencesHeader(res.headers.get('Sparkles-Inferences'));
  if (inferences) body.inferences = inferences;
  return normalizeResult(body);
}

/** Run a query with an arbitrary Accept header and return the raw response body. */
export async function queryRaw(
  ds: string,
  sparql: string,
  accept: string,
  opts: QueryOptions = {},
): Promise<Blob> {
  const res = await request(`/${enc(ds)}/sparql${queryParams(opts)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/sparql-query', Accept: accept },
    body: sparql,
    signal: opts.signal,
  });
  return res.blob();
}

/** Response of `/{ds}/update`: quad counts and server-side timing. */
export type UpdateResult = {
  inserted: number;
  deleted: number;
  operations: number;
  timing?: Timing;
  /** The commit the update produced (absent on servers that predate commits). */
  receipt?: Receipt;
};

/**
 * Run a SPARQL update. Asks for a commit receipt (`receipt=true`); servers that predate
 * receipts ignore the parameter.
 */
export async function update(
  ds: string,
  sparql: string,
  signal?: AbortSignal,
): Promise<UpdateResult | null> {
  const res = await request(`/${enc(ds)}/update?receipt=true`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/sparql-update', Accept: 'application/json' },
    body: sparql,
    signal,
  });
  // Fuseki answers with an HTML page; Sparkles with JSON counts.
  const text = await res.text();
  try {
    const body = JSON.parse(text);
    if (!body || typeof body.inserted !== 'number') return null;
    const receipt = receiptOf(body);
    return {
      inserted: body.inserted,
      deleted: body.deleted,
      operations: body.operations,
      timing: body.timing,
      ...(receipt ? { receipt } : {}),
    };
  } catch {
    return null;
  }
}

export function explain(
  ds: string,
  sparql: string,
  opts: { reasoning?: boolean; signal?: AbortSignal } = {},
): Promise<ExplainResult> {
  return json<ExplainResult>(`/${enc(ds)}/explain${queryParams({ reasoning: opts.reasoning })}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ query: sparql }).toString(),
    signal: opts.signal,
  });
}

/** Convenience: SELECT returning rows as objects keyed by variable name. */
export async function select(
  ds: string,
  sparql: string,
  opts: QueryOptions = {},
): Promise<Record<string, Term | undefined>[]> {
  const r = await query(ds, sparql, opts);
  const vars = r.vars ?? [];
  return (r.rows ?? []).map((row) =>
    Object.fromEntries(vars.map((v, i) => [v, row[i] ?? undefined])),
  );
}

function normalizeResult(r: SparklesResult): SparklesResult {
  // Be lenient about variable names with a leading '?' and missing meta.
  if (r.vars) r.vars = r.vars.map((v) => v.replace(/^[?$]/, ''));
  r.meta ??= {
    totalRows: r.rows?.length ?? r.triples?.length ?? 0,
    sentRows: r.rows?.length ?? r.triples?.length ?? 0,
    timing: { parseMs: 0, planMs: 0, execMs: 0, serializeMs: 0, totalMs: 0 },
    plan: undefined as unknown as PlanNode,
  };
  return r;
}

// --- SHACL --------------------------------------------------------------------

/** One `sh:ValidationResult` in the compact JSON report of `/{ds}/shacl`. */
export type ShaclResult = {
  focusNode: Term;
  resultPath: Term | { type: 'path'; value: string } | null;
  value: Term | null;
  sourceShape: Term;
  sourceConstraintComponent: Term;
  sourceConstraint?: Term;
  severity: Term;
  messages: string[];
};

export type ShaclReport = { conforms: boolean; results: ShaclResult[] };

export type ShaclOptions = {
  /** `default`, `union` or a graph IRI. */
  graph?: string;
  /** Include materialized inferences (server default: yes, when present). */
  reasoning?: boolean;
  signal?: AbortSignal;
};

function shaclRequest(
  ds: string,
  shapes: string,
  accept: string,
  opts: ShaclOptions,
): Promise<Response> {
  const p = new URLSearchParams();
  if (opts.graph) p.set('graph', opts.graph);
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  const qs = p.toString();
  return request(`/${enc(ds)}/shacl${qs ? `?${qs}` : ''}`, {
    method: 'POST',
    headers: { 'Content-Type': 'text/turtle', Accept: accept },
    body: shapes,
    signal: opts.signal,
  });
}

/** Validate a data graph against a Turtle shapes graph; compact JSON report. */
export async function shacl(
  ds: string,
  shapes: string,
  opts: ShaclOptions = {},
): Promise<ShaclReport> {
  const res = await shaclRequest(ds, shapes, 'application/json', opts);
  return (await res.json()) as ShaclReport;
}

/** Same validation, report in an RDF syntax (e.g. `text/turtle`) for download. */
export async function shaclRaw(
  ds: string,
  shapes: string,
  accept: string,
  opts: ShaclOptions = {},
): Promise<Blob> {
  const res = await shaclRequest(ds, shapes, accept, opts);
  return res.blob();
}

// --- upload -------------------------------------------------------------------

export type UploadProgress = { loaded: number; total: number };

/** Body of a successful upload. */
export type UploadResult = {
  count?: number;
  tripleCount?: number;
  quadCount?: number;
  /** The commit the upload produced (absent on servers that predate commits). */
  receipt?: Receipt;
};

/**
 * Multipart upload to /{ds}/upload with progress reporting (XHR, since fetch has no upload
 * progress). Asks for a commit receipt (`receipt=true`).
 */
export function upload(
  ds: string,
  files: File[],
  opts: { graph?: string; onProgress?: (p: UploadProgress) => void; signal?: AbortSignal } = {},
): Promise<UploadResult | string> {
  return new Promise((resolve, reject) => {
    const form = new FormData();
    if (opts.graph) form.append('graph', opts.graph);
    for (const f of files) form.append('file', f, f.name);
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `/${enc(ds)}/upload?receipt=true`);
    xhr.setRequestHeader('Accept', 'application/json');
    const csrf = authHooks.csrf();
    if (csrf) xhr.setRequestHeader(CSRF_HEADER, csrf);
    xhr.upload.onprogress = (e) =>
      opts.onProgress?.({ loaded: e.loaded, total: e.lengthComputable ? e.total : 0 });
    xhr.onerror = () => reject(new ApiError(0, 'Upload failed: network error'));
    xhr.onabort = () => reject(new DOMException('Upload cancelled', 'AbortError'));
    xhr.onload = () => {
      let body: unknown = xhr.responseText;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        /* text body */
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        if (!body || typeof body !== 'object') return resolve(String(body ?? ''));
        const receipt = receiptOf(body as Record<string, unknown>);
        return resolve({ ...(body as UploadResult), receipt });
      }
      const b = body as { error?: string; detail?: string; line?: number; column?: number };
      reject(
        new ApiError(xhr.status, b?.error ?? `Upload failed (${xhr.status})`, {
          detail: b?.detail,
          line: b?.line,
          column: b?.column,
          requestId: xhr.getResponseHeader('X-Request-Id') ?? undefined,
        }),
      );
    };
    opts.signal?.addEventListener('abort', () => xhr.abort());
    xhr.send(form);
  });
}

export function errorMessage(e: unknown): string {
  if (e instanceof ApiError)
    return e.detail && e.detail !== e.message ? `${e.message}: ${e.detail}` : e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

// --- reasoning status and diagnostics -------------------------------------------

/** `GET /$/reason/{ds}`: the recorded reasoning and whether it is up to date. */
export type ReasoningStatus = {
  profile: string;
  inferred: number;
  at: string;
  /** Commit the inferences were materialized at (null: unknown, e.g. older status). */
  commit: number | null;
  head: number;
  /** null: freshness unknown. */
  stale: boolean | null;
  commitsSince: number | null;
  staleReason?: string;
  auto: { enabled: boolean; debounceSeconds?: number; scheduledAt?: string };
  warnings: string[];
};

/** Parsed `Sparkles-Inferences` response header (sent only when not fresh). */
export type InferencesNotice = { stale: boolean | null; commitsSince: number | null };

export function parseInferencesHeader(v: string | null): InferencesNotice | undefined {
  if (!v) return undefined;
  const [state, ...params] = v.split(';').map((p) => p.trim());
  const since = params.find((p) => p.startsWith('commits-since='));
  const n = since ? Number(since.slice('commits-since='.length)) : NaN;
  return {
    stale: state === 'stale' ? true : null,
    commitsSince: Number.isFinite(n) ? n : null,
  };
}

export async function reasonStatus(ds: string): Promise<ReasoningStatus | null> {
  const body = await json<ReasoningStatus | { reasoning: null; head: number }>(
    `/$/reason/${enc(ds)}`,
  );
  return body && 'profile' in body ? body : null;
}

/** Re-run the recorded profile (including custom rules). */
export const rerunReasoning = (ds: string) =>
  json<Task>(`/$/reason/${enc(ds)}`, jsonBody({ rerun: true }));

export const DIAGNOSTIC_CHECKS = [
  'nothing-member',
  'disjoint-classes',
  'all-disjoint-classes',
  'same-different',
  'functional-literal-conflict',
  'thing-empty',
  'unsatisfiable-class',
] as const;
export type DiagnosticCheck = (typeof DIAGNOSTIC_CHECKS)[number];

export type DiagnosticsReport = {
  diagnosticsFormat: 1;
  dataset: string;
  commit: number;
  computedAt: string;
  scope: {
    graph: 'default';
    inferences: {
      included: boolean;
      profile?: string;
      stale?: boolean | null;
      commitsSince?: number | null;
    };
    closure: 'subclass' | 'none';
  };
  status: 'violations-found' | 'none-found' | 'incomplete';
  note: string;
  checks: {
    id: string;
    rules: string[];
    severity: 'inconsistency' | 'warning';
    status: 'violations' | 'none' | 'truncated' | 'timeout' | 'error';
    findings: number;
    millis: number;
    error?: string;
  }[];
  findings: {
    check: string;
    rule: string;
    severity: 'inconsistency' | 'warning';
    focus: Term;
    evidence: Record<string, Term | Term[]>;
    basis: 'asserted' | 'uses-inferences';
    message: string;
  }[];
};

export type DiagnosticsOptions = {
  checks?: string[];
  limit?: number;
  reasoning?: boolean;
  closure?: 'subclass' | 'none';
  signal?: AbortSignal;
};

/** `GET /$/reason/{ds}/diagnostics`: OWL 2 RL inconsistency checks. */
export function diagnostics(ds: string, opts: DiagnosticsOptions = {}): Promise<DiagnosticsReport> {
  const p = new URLSearchParams();
  if (opts.checks?.length) p.set('checks', opts.checks.join(','));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  if (opts.closure) p.set('closure', opts.closure);
  const qs = p.toString();
  return json<DiagnosticsReport>(`/$/reason/${enc(ds)}/diagnostics${qs ? `?${qs}` : ''}`, {
    signal: opts.signal,
  });
}

// --- clone ------------------------------------------------------------------------

export type DatasetOrigin = {
  originFormat: 1;
  clonedAt: string;
  source: { name: string; path?: string; version: number; generation: string; quads: number };
  forkedFrom: { id: string; seq: number };
  inferences: 'copy' | 'drop';
};

/** Copy one snapshot of `ds` into the new persistent dataset `name` (a task). */
export const cloneDataset = (ds: string, name: string, inferences: 'copy' | 'drop' = 'copy') =>
  json<Task>(`/$/datasets/${enc(ds)}/clone`, jsonBody({ name, inferences }));

// --- commits ----------------------------------------------------------------------

export type CommitKind =
  | 'create'
  | 'baseline'
  | 'update'
  | 'gsp-put'
  | 'gsp-post'
  | 'gsp-delete'
  | 'upload'
  | 'load'
  | 'reason'
  | 'reason-clear'
  | 'transaction'
  | 'unknown';

/** One commit: the state after a write that changed data. */
export type Commit = {
  seq: number;
  parent: number | null;
  /** `commit:42` */
  ref: string;
  /** RFC 3339 UTC with milliseconds, never decreasing along the sequence. */
  timestamp: string;
  kind: CommitKind;
  /** Net change relative to the parent. */
  inserted: number;
  deleted: number;
  /** Dataset size after the commit. */
  quads: number;
  /** Index generation the commit was made in. */
  generation: string;
  /** Made by rebuilding the index. */
  bulk: boolean;
  /** false: a bulk commit that also deleted, whose counts may include a quad twice. */
  exact: boolean;
  /** Rebuilt from a write-ahead log record without commit metadata. */
  reconstructed?: boolean;
};

/** What a write produced: the new commit, or the unchanged head when nothing changed. */
export type Receipt = {
  dataset: string;
  datasetId: string;
  committed: boolean;
  commit: Commit;
};

/** The receipt members of a write response body, if it has them. */
export function receiptOf(body: Record<string, unknown> | null | undefined): Receipt | undefined {
  if (!body || typeof body.committed !== 'boolean') return undefined;
  const c = body.commit as Commit | undefined;
  if (!c || typeof c !== 'object' || typeof c.seq !== 'number') return undefined;
  return {
    dataset: String(body.dataset ?? ''),
    datasetId: String(body.datasetId ?? ''),
    committed: body.committed,
    commit: c,
  };
}

/** `GET /$/commits/{ds}`: a page of the commit catalog, newest first. */
export type CommitPage = {
  dataset: string;
  datasetId: string;
  head: number;
  /** Oldest commit whose metadata is still available. */
  firstRetained: number;
  /** false while the catalog lags the write-ahead log after a write error. */
  complete: boolean;
  commits: Commit[];
  /** URL of the next (older) page, or null. */
  next: string | null;
};

/** Newest commits first; `before` pages backwards from a sequence number. */
export function commits(
  ds: string,
  opts: { before?: number; limit?: number; signal?: AbortSignal } = {},
): Promise<CommitPage> {
  const p = new URLSearchParams();
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.before != null) p.set('before', String(opts.before));
  const qs = p.toString();
  return json<CommitPage>(`/$/commits/${enc(ds)}${qs ? `?${qs}` : ''}`, {
    signal: opts.signal,
    cache: 'no-store',
  });
}

// --- full-text search --------------------------------------------------------------

export type TextState = 'ready' | 'stale' | 'failed';

export type TextConfig = {
  /** Default "all". */
  predicates?: 'all' | string[];
  graphs?: { include?: 'all' | string[]; exclude?: string[] };
  maxTextBytes?: number;
  maxHits?: number;
};

export type TextStatus = {
  enabled: true;
  state: TextState;
  docs: number;
  /** The commit the index reflects; ready when equal to `storeSeq`. */
  seq: number;
  storeSeq: number;
  epoch: number;
  diskBytes: number;
  segments: number;
  config: TextConfig;
  formatVersion: number;
  lastRebuild?: { at: string; ms: number; docs: number };
  message?: string;
};

/**
 * `GET /$/text/{ds}`: the index status, or null when full-text search is disabled for
 * the dataset. Throws an ApiError with status 501 on servers built without it.
 */
export async function textStatus(ds: string, signal?: AbortSignal): Promise<TextStatus | null> {
  const body = await json<TextStatus | { enabled: false }>(`/$/text/${enc(ds)}`, {
    signal,
    cache: 'no-store',
  });
  return body && body.enabled ? body : null;
}

/** Enable or reconfigure full-text search; the index is built by the returned task. */
export const enableText = (ds: string, config: TextConfig = {}) =>
  json<Task>(`/$/text/${enc(ds)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(config),
  });

/** Disable full-text search and delete the index. */
export const disableText = (ds: string) =>
  json<unknown>(`/$/text/${enc(ds)}`, { method: 'DELETE' });

/** Rebuild the index from the current data (`409` while a rebuild runs). */
export const rebuildText = (ds: string) =>
  json<Task>(`/$/text/${enc(ds)}/rebuild`, { method: 'POST' });
