// Typed client for the Sparkles HTTP API (see docs/API.md — the source of truth).
// All paths are absolute from the server root; the UI itself lives under /ui/.

import { CSRF_HEADER, needsCsrf, type Level } from './auth';
import { fmtBytes, fmtInt } from './format';

export type DatasetType = 'persistent' | 'mem';

export type DatasetInfo = {
  name: string;
  type: DatasetType;
  endpoints: {
    query: string;
    update: string;
    gsp: string;
    upload: string;
    shacl?: string;
    shex?: string;
  };
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
  /** Spatial index summary: state and indexed rows (null: disabled; absent: server without the field). */
  geo?: { state: GeoState; rows: number } | null;
  /** The caller's level on the dataset; absent without auth (then everything goes). */
  access?: Level;
};

/** Per-request budgets of the server; 0 means unlimited. */
export type Limits = {
  timeoutSeconds: number;
  /** Absent on servers that predate it. */
  updateTimeoutSeconds?: number;
  /** The largest `timeout` a request may ask for; absent on servers that predate it. */
  maxTimeoutSeconds?: number;
  queryMemoryBytes: number;
  /** The serialized body of a SPARQL query response. */
  maxResultBytes: number;
  /** The serialized body of a Graph Store GET; absent on servers that predate it. */
  maxExportBytes?: number;
  maxRows: number;
  /** Rows all the operators of one query produce; absent on servers that predate it. */
  maxRowsProduced?: number;
  /** The default storage quota of a persistent dataset; absent on servers that predate it. */
  maxDatasetBytes?: number;
};

export type ServerInfo = {
  /** Absent for anonymous callers of a server with authentication. */
  version?: string;
  startedAt: string;
  uptimeSeconds: number;
  datasets: DatasetInfo[];
  /** Absent on servers that predate budgets, and for anonymous callers with auth. */
  limits?: Limits;
  /** Absent on servers that predate it. */
  readOnly?: boolean;
  /** The MapLibre style of maps (`serve --map-style-url`); null: the bundled basemap. */
  mapStyleUrl?: string | null;
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
export type LimitClass = 'auth' | 'query' | 'update' | 'admin' | 'preauth';
export type BudgetKind =
  | 'rows'
  | 'memory'
  | 'result-bytes'
  | 'decompressed-bytes'
  | 'outbound-bytes'
  | 'rows-produced'
  | 'dataset-bytes';

type CacheStats = {
  bytes: number;
  capacityBytes: number;
  entries: number;
  hits: number;
  misses: number;
};

/** `GET /$/quota/{ds}`: a dataset's storage quota and the bytes it uses. */
export type DatasetQuota = {
  /** The quota in effect; null when unlimited. */
  maxBytes: number | null;
  /** `dataset`: set on the dataset; `default`: the server's `--max-dataset-mb`. */
  source: 'dataset' | 'default';
  defaultMaxBytes: number | null;
  usedBytes: number;
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
  /** Storage quota of a persistent dataset (null in memory); absent on servers that predate it. */
  quota?: DatasetQuota | null;
  /** Decoded-block cache. */
  cache: { entries: number; bytes: number; hits: number; misses: number };
  /** Query (sub)result cache; absent on servers that predate it. */
  resultCache?: { enabled: boolean; entries: number; bytes: number; hits: number; misses: number };
  /** Reasoning status; absent on servers that predate it. */
  reasoning?: ReasoningStatus | null;
  /** Spatial index status (null: disabled); absent on servers that predate it. */
  geo?: GeoStatus | null;
  /** The commit the statistics describe; absent on servers that predate it. */
  commit?: number;
  /** The selector of a past state (`commit:42`), or null at the head. */
  at?: string | null;
  history?: {
    bytes: number;
    generations: number;
    snapshots: number;
    oldestReconstructable: number | null;
  };
};

export type TaskKind =
  | 'compact'
  | 'backup'
  | 'reason'
  | 'load'
  | 'clone'
  | 'text-rebuild'
  | 'geo-index'
  | 'backup-create'
  | 'backup-restore'
  | 'backup-verify'
  | 'backup-gc'
  | 'backup-policy';
/** `queued`: waiting for a task slot; `cancelled`: stopped by `DELETE /$/tasks/{id}`. */
export type TaskState = 'queued' | 'running' | 'done' | 'failed' | 'cancelled';
export type Task = {
  id: string;
  kind: TaskKind;
  /** The dataset the task works on; `""` for server-scoped tasks (repository verify, GC, policy runs). */
  dataset: string;
  /** The dataset a task creates (clone, restore), or the backup, repository or policy it works on. */
  target?: string;
  state: TaskState;
  startedAt: string;
  finishedAt?: string;
  message?: string;
  progress?: number;
  /** `DELETE /$/tasks/{id}` may stop it; absent on servers that predate cancellation. */
  cancellable?: boolean;
  /** The typed result of a finished task (a backup summary, verify or GC report, policy run). */
  detail?: unknown;
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
  /** Operator counters (spatial operators: candidates, refined, matched, treeNodesVisited, index, fallback; expressions per distinct value: exprCacheHits, exprCacheMisses, exprCacheSkipped). */
  counters?: Record<string, number | string | boolean>;
  /** Notes about the plan (root only). */
  warnings?: PlanWarning[];
};

/** `geo-not-pushed`, `geo-index-building`, `geo-not-built`, … */
export type PlanWarning = { code: string; message: string };

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
  /** From the `Sparkles-At` headers: the state a query with `at` read. */
  at?: AtInfo;
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
  /** the machine-readable error code of bodies that have one (`syntax`, `bad-request` …) */
  code?: string;
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
  code?: string;
  constructor(status: number, message: string, extra: ApiErrorExtra = {}) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.detail = extra.detail;
    this.line = extra.line;
    this.column = extra.column;
    this.requestId = extra.requestId;
    this.budget = extra.budget;
    this.code = extra.code;
  }
}

function budgetOf(body: Record<string, unknown>): Budget | undefined {
  const kind = body.budget;
  if (
    kind !== 'rows' &&
    kind !== 'memory' &&
    kind !== 'result-bytes' &&
    kind !== 'decompressed-bytes' &&
    kind !== 'outbound-bytes' &&
    kind !== 'rows-produced' &&
    kind !== 'dataset-bytes'
  )
    return undefined;
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
    case 'decompressed-bytes':
      return `The request body is too large once decompressed (limit ${fmtBytes(b.limit)}). Send less data per request.`;
    case 'outbound-bytes':
      return `SERVICE calls and LOADs downloaded too much in total (limit ${fmtBytes(b.limit)}). Fetch less remote data per request.`;
    case 'rows-produced':
      return `The query does too much work (limit ${fmtInt(b.limit)} rows produced). Narrow the query or make its patterns more selective.`;
    case 'dataset-bytes':
      return `The write would take the dataset over its storage quota (${fmtBytes(b.limit)}). Delete data, compact the dataset, or ask an administrator for a larger quota.`;
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
        code: typeof body.code === 'string' ? body.code : undefined,
      });
    }
  } catch {
    /* not JSON */
  }
  const fallback = text.trim().slice(0, 500) || res.statusText || 'Request failed';
  return new ApiError(res.status, `${res.status} ${fallback}`, { requestId });
}

/**
 * How requests authenticate: the CSRF token of the session, what a 401 does, and the first
 * load of the caller (`/$/whoami`, which brings the CSRF token).
 */
const authHooks: {
  csrf: () => string | undefined;
  unauthorized: () => void;
  ready: () => Promise<void>;
} = {
  csrf: () => undefined,
  unauthorized: () => {},
  ready: async () => {},
};

export function setAuthHooks(h: Partial<typeof authHooks>) {
  Object.assign(authHooks, h);
}

/**
 * The CSRF token for a request with this method, if it needs one. An unsafe request made
 * before the caller is known (a page that writes or queries by POST as soon as it opens)
 * waits for it instead of going out without the header.
 */
async function csrfFor(method: string | undefined): Promise<string | undefined> {
  if (!needsCsrf(method)) return undefined;
  if (authHooks.csrf() === undefined) await authHooks.ready();
  return authHooks.csrf();
}

/** `init` with the CSRF header added to unsafe requests of a session. */
async function withCsrf(init: RequestInit): Promise<RequestInit> {
  const token = await csrfFor(init.method);
  if (!token) return init;
  const headers = new Headers(init.headers);
  headers.set(CSRF_HEADER, token);
  return { ...init, headers };
}

export async function request(path: string, init: RequestInit = {}): Promise<Response> {
  const withToken = await withCsrf(init);
  let res: Response;
  try {
    res = await fetch(path, withToken);
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

// --- formatting ---------------------------------------------------------------

/** The languages `POST /$/format` knows (only `sparql` formats so far). */
export type FormatLanguage = 'sparql' | 'turtle' | 'trig' | 'ntriples' | 'nquads' | 'jsonld';

/** The style options (the `.sparklesfmt.toml` keys in camelCase); omitted ones take the defaults. */
export type FormatOptions = {
  lineWidth?: number;
  indentWidth?: number;
  sort?: boolean;
  prunePrefixes?: boolean;
  directiveStyle?: 'sparql' | 'turtle';
  prefixGroups?: string[][];
  typeShorthand?: boolean;
  compactIris?: boolean;
  quoteStyle?: 'double' | 'preserve';
  operatorPosition?: 'leading' | 'trailing';
  turtleLayout?: 'diff' | 'conventional';
  alignValues?: boolean;
};

export type FormatRequest = {
  text: string;
  /** sniffed from the text when absent */
  language?: FormatLanguage;
  /** the cursor, in UTF-16 code units (the editor's unit) */
  cursorOffset?: number;
  options?: FormatOptions;
};

export type FormatWarning = { code: string; message: string; line: number; column: number };

export type FormatResult = {
  text: string;
  changed: boolean;
  language: FormatLanguage;
  /** the cursor mapped into `text` (UTF-16 code units), `null` without one */
  cursorOffset: number | null;
  warnings: FormatWarning[];
};

/**
 * Format a document. Throws an `ApiError`: `400` with `code` `syntax` and the error's `line`
 * and `column`, `400` `bad-request`, `415` for a language that does not format, `422` when the
 * formatter refuses its own output (`unsafe-format`, `unstable-format`, `unsupported-syntax`).
 */
export const format = (req: FormatRequest, signal?: AbortSignal) =>
  json<FormatResult>('/$/format', { ...jsonBody(req), signal });

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

/** `GET /$/stats/{ds}`, of the state `at` selects when given. */
export const datasetStats = (ds: string, at?: string) =>
  json<DatasetStats>(`/$/stats/${enc(ds)}${at ? `?at=${enc(at)}` : ''}`);

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
  /** Read a past state: `head`, `42`, `commit:42`, `time:<RFC 3339>`, `snapshot:NAME`. */
  at?: string;
  signal?: AbortSignal;
};

function queryParams(opts: QueryOptions): string {
  const p = new URLSearchParams();
  if (opts.at) p.set('at', opts.at);
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
  const at = atInfo(res.headers);
  if (at) body.at = at;
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

// --- ShEx ---------------------------------------------------------------------

/** Why a node does not conform to a shape (`appinfo.failures` of a ShEx result). */
export type ShexFailure =
  | { kind: 'nodeKind' | 'datatype' | 'facet' | 'valueSet'; value: Term; constraint: string }
  | {
      kind: 'cardinality';
      predicate: string;
      inverse: boolean;
      min: number;
      max: number | null;
      count: number;
    }
  | { kind: 'closed' | 'extra'; predicate: string; value: Term }
  | { kind: 'noMatch'; detail: string }
  | { kind: 'reference'; shape: string; value: Term }
  | { kind: 'not' | 'external'; shape: string }
  | { kind: 'semAct'; extension: string; message: string };

/** One association of the result map of `/{ds}/shex`. */
export type ShexResult = {
  node: Term;
  shape: Term | { type: 'start' };
  status: 'conformant' | 'nonconformant';
  /** The first failure, in one line. */
  reason?: string;
  appinfo?: { failures: ShexFailure[]; prints?: string[] };
};

export type ShexReport = {
  conforms: boolean;
  counts: { conformant: number; nonconformant: number };
  results: ShexResult[];
  warnings: string[];
  millis: number;
};

export type ShexOptions = {
  /** `default`, `union` or a graph IRI. */
  graph?: string;
  /** Include materialized inferences (server default: yes, when present). */
  reasoning?: boolean;
  /** Report only nonconformant associations (the counts still cover all). */
  onlyNonconformant?: boolean;
  /** Base IRI of the schema's (and the shape map's) relative IRIs. */
  base?: string;
  signal?: AbortSignal;
};

/** Report formats of `/{ds}/shex` besides the JSON report. */
export type ShexFormat = 'shapemap' | 'smap' | 'text';

function shexRequest(
  ds: string,
  schema: string,
  map: string,
  format: 'json' | ShexFormat,
  opts: ShexOptions,
): Promise<Response> {
  const p = new URLSearchParams();
  if (opts.graph) p.set('graph', opts.graph);
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  if (opts.onlyNonconformant) p.set('results', 'nonconformant');
  if (opts.base) p.set('base', opts.base);
  if (format !== 'json') p.set('format', format);
  const qs = p.toString();
  return request(`/${enc(ds)}/shex${qs ? `?${qs}` : ''}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json, text/plain' },
    // the envelope: the schema is sniffed (ShExJ when it starts with `{`, else ShExC)
    body: JSON.stringify({ schema, map }),
    signal: opts.signal,
  });
}

/** Validate a data graph against a ShEx schema (ShExC or ShExJ) and a compact shape map. */
export async function shex(
  ds: string,
  schema: string,
  map: string,
  opts: ShexOptions = {},
): Promise<ShexReport> {
  const res = await shexRequest(ds, schema, map, 'json', opts);
  return (await res.json()) as ShexReport;
}

/** Same validation, the result map in another format (e.g. `shapemap` JSON) for download. */
export async function shexRaw(
  ds: string,
  schema: string,
  map: string,
  format: ShexFormat,
  opts: ShexOptions = {},
): Promise<Blob> {
  const res = await shexRequest(ds, schema, map, format, opts);
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
export async function upload(
  ds: string,
  files: File[],
  opts: { graph?: string; onProgress?: (p: UploadProgress) => void; signal?: AbortSignal } = {},
): Promise<UploadResult | string> {
  const csrf = await csrfFor('POST');
  return new Promise((resolve, reject) => {
    const form = new FormData();
    if (opts.graph) form.append('graph', opts.graph);
    for (const f of files) form.append('file', f, f.name);
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `/${enc(ds)}/upload?receipt=true`);
    xhr.setRequestHeader('Accept', 'application/json');
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
  auto: {
    enabled: boolean;
    /** `server`: --auto-reason; `dataset`: the dataset's own setting */
    source?: 'server' | 'dataset';
    debounceSeconds?: number;
    maxDelaySeconds?: number;
    scheduledAt?: string;
  };
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

/** The dataset's own automatic re-run setting (`PUT /$/reason/{ds}/auto`). */
export const setAutoReasoning = (ds: string, enabled: boolean) =>
  json<ReasoningStatus>(`/$/reason/${enc(ds)}/auto`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ enabled }),
  });

/** Drop the dataset's own setting: the server's `--auto-reason` applies again. */
export const clearAutoReasoning = (ds: string) =>
  json<ReasoningStatus>(`/$/reason/${enc(ds)}/auto`, { method: 'DELETE' });

/** Re-run the recorded profile (including custom rules). */
export const rerunReasoning = (ds: string) =>
  json<Task>(`/$/reason/${enc(ds)}`, jsonBody({ rerun: true }));

export const DIAGNOSTIC_CHECKS = [
  'nothing-member',
  'disjoint-classes',
  'all-disjoint-classes',
  'complement-classes',
  'max-cardinality-zero',
  'max-qualified-cardinality-zero',
  'same-different',
  'all-different',
  'functional-literal-conflict',
  'irreflexive-property',
  'asymmetric-property',
  'disjoint-properties',
  'all-disjoint-properties',
  'negative-property-assertion',
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
  /** A point-in-time read (`at`) can still see this commit; absent on older servers. */
  reconstructable?: boolean;
  /** Named snapshots that pin this commit. */
  snapshots?: string[];
  /** The message recorded with the commit. */
  message?: string;
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
  /** Oldest commit a point-in-time read can see; absent on older servers. */
  oldestReconstructable?: number | null;
  reconstructable?: { from: number; to: number }[];
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

// --- spatial index (GeoSPARQL) -----------------------------------------------------

export type GeoState = 'ready' | 'building' | 'failed' | 'over-budget';

export type GeoConfig = {
  /** Serialization predicates; default geo:asWKT, geo:asGeoJSON, geo:hasSerialization. */
  predicates?: string[];
  /** Default geo:hasDefaultGeometry, geo:hasGeometry. */
  featureLinks?: string[];
  graphs?: { include?: 'all' | string[]; exclude?: string[] };
  /** Index W3C Basic Geo latitude/longitude pairs as points; default false. */
  wgs84?: boolean;
  /** Default false: the topological geo: properties also match derived triples (query rewrite). */
  queryRewrite?: boolean;
  /** Default "geodesic". */
  distance?: 'geodesic' | 'haversine';
  maxGeometryBytes?: number;
  maxVertices?: number;
  formatVersion?: number;
};

export type GeoStatus = {
  enabled: true;
  state: GeoState;
  progress?: number;
  message?: string;
  /** The generation the base was built for. */
  generation: string;
  /** The commit of the snapshot the status describes. */
  commit: number;
  /** `wgs84`: the rows (of the three) that are W3C Basic Geo points, when any. */
  rows: { base: number; overlay: number; tail: number; wgs84?: number };
  /** Distinct parsed geometries. */
  literals: number;
  skipped: { malformed: number; unknownCrs: number; tooLarge: number; empty: number };
  /** Literals per CRS IRI. */
  crs: Record<string, number>;
  /** `mappedBytes`: index files read in place (not counted against the budget). */
  memory: {
    treeBytes: number;
    geometryBytes: number;
    overlayBytes: number;
    budgetBytes: number;
    mappedBytes?: number;
  };
  config: GeoConfig;
  formatVersion: number;
  lastBuild?: { at: string; ms: number; rows: number };
  /** The index files of the base (persistent stores); `opened`: read, not built. */
  files?: { bytes: number; opened: boolean };
};

/**
 * `GET /$/geo/{ds}`: the spatial index status, or null when the index is disabled for
 * the dataset. Throws an ApiError with status 501 on servers built without GeoSPARQL.
 */
export async function geoStatus(ds: string, signal?: AbortSignal): Promise<GeoStatus | null> {
  const body = await json<GeoStatus | { enabled: false }>(`/$/geo/${enc(ds)}`, {
    signal,
    cache: 'no-store',
  });
  return body && body.enabled ? body : null;
}

/** Enable or reconfigure the spatial index; the index is built by the returned task. */
export const geoConfigure = (ds: string, config: GeoConfig = {}) =>
  json<Task>(`/$/geo/${enc(ds)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(config),
  });

/** Disable the spatial index (removes geo.json). */
export const geoDisable = (ds: string) => json<unknown>(`/$/geo/${enc(ds)}`, { method: 'DELETE' });

/** Rebuild the index from the current data (`409` while a build runs). */
export const geoRebuild = (ds: string) =>
  json<Task>(`/$/geo/${enc(ds)}/rebuild`, { method: 'POST' });

/** A GeoJSON geometry in CRS84 (longitude, latitude). */
export type GeoJsonGeometry = {
  type: string;
  coordinates?: unknown;
  geometries?: GeoJsonGeometry[];
};

/** An indexed geometry of `GET /{ds}/geo`; `id` is the row's subject. */
export type GeoFeature = {
  type: 'Feature';
  id: string;
  geometry: GeoJsonGeometry;
  properties: { subject: string; feature?: string; graph: string | null; predicate: string };
};

export type GeoFeatureCollection = {
  type: 'FeatureCollection';
  features: GeoFeature[];
  /** More geometries meet the box than `limit`. */
  truncated: boolean;
};

export type GeoBoxQuery = {
  /** [minLon, minLat, maxLon, maxLat] in degrees. */
  bbox: [number, number, number, number];
  graph?: string;
  predicate?: string;
  /** Default 5,000, at most 50,000. */
  limit?: number;
  /** Simplification tolerance in degrees; default the box width / 1024. */
  tolerance?: number;
};

/** `GET /{ds}/geo`: the indexed geometries meeting a box, simplified for its scale. */
export function geoBox(ds: string, q: GeoBoxQuery, signal?: AbortSignal) {
  const p = new URLSearchParams({ bbox: q.bbox.join(',') });
  if (q.graph) p.set('graph', q.graph);
  if (q.predicate) p.set('predicate', q.predicate);
  if (q.limit != null) p.set('limit', String(q.limit));
  if (q.tolerance != null) p.set('tolerance', String(q.tolerance));
  return json<GeoFeatureCollection>(`/${enc(ds)}/geo?${p}`, { signal });
}

/** One literal's outcome of `POST /$/geo/convert`. */
export type GeoConverted = { geometry: GeoJsonGeometry } | { error: string };

/** `POST /$/geo/convert`: geometry literals as CRS84 GeoJSON, in order. */
export const geoConvert = (literals: { value: string; datatype: string }[], signal?: AbortSignal) =>
  json<{ results: GeoConverted[] }>(`/$/geo/convert`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ literals }),
    signal,
  });

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

// --- point-in-time reads, named snapshots and diffs ------------------------------

/** The state a read with `at` saw, from its response headers. */
export type AtInfo = {
  /** canonical selector: `head`, `commit:42`, `time:…`, `snapshot:NAME` */
  selector: string;
  commit: number | null;
  head: number | null;
  /** a past state (the server sent `Memento-Datetime`) */
  historical: boolean;
  /** `Memento-Datetime` as an ISO time, for a past state */
  datetime: string | null;
};

/** The `Sparkles-At` family of headers, or undefined when the read had no `at`. */
export function atInfo(h: Headers): AtInfo | undefined {
  const selector = h.get('Sparkles-At');
  if (!selector) return undefined;
  const num = (v: string | null) => (v != null && /^\d+$/.test(v) ? Number(v) : null);
  const memento = h.get('Memento-Datetime');
  const t = memento ? new Date(memento) : null;
  return {
    selector,
    commit: num(h.get('Sparkles-Commit')),
    head: num(h.get('Sparkles-Head')),
    historical: memento != null,
    datetime: t && !Number.isNaN(t.getTime()) ? t.toISOString() : null,
  };
}

/** A named snapshot: a durable name for a commit that keeps it readable. */
export type NamedSnapshot = {
  name: string;
  /** `snapshot:NAME` */
  ref: string;
  seq: number;
  /** the pinned commit's metadata (null once the catalog no longer has it) */
  commit: Commit | null;
  created: string;
  /** when the pin lapses, or null */
  expires: string | null;
  note: string | null;
  generation: string | null;
  reconstructable: boolean;
};

export type SnapshotList = {
  dataset: string;
  datasetId: string;
  head: number;
  snapshots: NamedSnapshot[];
};

/** `GET /$/snapshots/{ds}`, sorted by commit then name. */
export const snapshots = (ds: string, signal?: AbortSignal) =>
  json<SnapshotList>(`/$/snapshots/${enc(ds)}`, { signal, cache: 'no-store' });

/**
 * `POST /$/snapshots/{ds}`: pin `at` (default the head) as `name`. `expires` is an RFC 3339
 * time or a duration from now (`90s`, `30m`, `12h`, `7d`, `2w`). Creating a name that
 * already pins the same commit succeeds again; another commit is a 409.
 */
export const createSnapshot = (
  ds: string,
  body: { name: string; at?: string; note?: string; expires?: string },
) => json<NamedSnapshot>(`/$/snapshots/${enc(ds)}`, jsonBody(body));

/** `DELETE /$/snapshots/{ds}/{name}`; the history only it kept is collected. */
export const deleteSnapshot = (ds: string, name: string) =>
  json<unknown>(`/$/snapshots/${enc(ds)}/${enc(name)}`, { method: 'DELETE' });

export type Retention = {
  keepCommits: number | null;
  /** seconds, as `"86400s"` */
  keepAge: string | null;
  maxBytes?: number | null;
};

export type PinSchedule = { prefix: string; every: string; keepLast: number };

/** `GET /$/history/{ds}`: retained generations, readable commits and retention. */
export type HistoryStatus = {
  dataset: string;
  datasetId: string;
  head: number;
  oldestReconstructable: number | null;
  reconstructable: { from: number; to: number }[];
  bytes: number;
  generations: {
    name: string;
    baseSeq: number;
    endSeq: number;
    bytes: number;
    current: boolean;
    heldBy: string[];
  }[];
  retention: Retention;
  schedules?: PinSchedule[];
  snapshots: number;
};

export const history = (ds: string, signal?: AbortSignal) =>
  json<HistoryStatus>(`/$/history/${enc(ds)}`, { signal, cache: 'no-store' });

/** `PUT /$/history/{ds}`: the retention window, and the pin schedules when given. */
export const setHistory = (
  ds: string,
  body: {
    keepCommits?: number | null;
    keepAge?: string | number | null;
    maxBytes?: string | number | null;
    schedules?: PinSchedule[] | null;
  },
) =>
  json<HistoryStatus>(`/$/history/${enc(ds)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });

export type DiffQuad = {
  op: '+' | '-';
  subject: string;
  predicate: string;
  object: string;
  /** null for the default graph */
  graph: string | null;
};

/** `GET /{ds}/diff`: the net change from one state to another. */
export type Diff = {
  dataset: string;
  datasetId: string;
  from: { selector: string; commit: Commit };
  to: { selector: string; commit: Commit };
  added: number;
  removed: number;
  /** `log` (read from the write-ahead logs), `compare` (state against state) or `same` */
  method: 'log' | 'compare' | 'same';
  /** with `quads`: removals first, then additions */
  quads?: DiffQuad[];
};

export type DiffOptions = {
  /** default: the commit before `to` */
  from?: string;
  /** default: the head */
  to?: string;
  /** an IRI, or `default` */
  graph?: string;
  /** list the changed quads (at most `limit` of them) */
  quads?: boolean;
  limit?: number;
  signal?: AbortSignal;
};

export function diff(ds: string, opts: DiffOptions = {}): Promise<Diff> {
  const p = new URLSearchParams();
  if (opts.from) p.set('from', opts.from);
  if (opts.to) p.set('to', opts.to);
  if (opts.graph) p.set('graph', opts.graph);
  if (opts.quads) p.set('quads', 'true');
  if (opts.limit != null) p.set('limit', String(opts.limit));
  const qs = p.toString();
  return json<Diff>(`/${enc(ds)}/diff${qs ? `?${qs}` : ''}`, { signal: opts.signal });
}
