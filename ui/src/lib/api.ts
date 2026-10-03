// Typed client for the Sparkles HTTP API (see docs/API.md — the source of truth).
// All paths are absolute from the server root; the UI itself lives under /ui/.

import { CSRF_HEADER, needsCsrf, type Level } from './auth';
import { fmtBytes, fmtInt } from './format';
import { cloneBody, type CloneMethod, type CloneOptions } from './clone';
import { mappingPart, tableParams } from './upload';

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

/** The settings of a dataset's automatic compaction (`0` turns a size, idle or age trigger off). */
export type CompactionPolicy = {
  enabled: boolean;
  minDeltaQuads: number;
  deltaRatio: number;
  maxDeltaQuads: number;
  maxDeltaMb: number;
  maxWalMb: number;
  idleSeconds: number;
  maxAgeSeconds: number;
  minIntervalSeconds: number;
  /** Whether a compaction may rewrite only the index blocks its delta touches. */
  partial: 'auto' | 'off' | 'always';
};

/** How DESCRIBE describes a resource: `cbd`, `scbd` or `outgoing`. */
export type DescribeMode = 'cbd' | 'scbd' | 'outgoing';

/** The options of a dataset's DESCRIBE setting (`null` limits: none). */
export type DescribeOptions = {
  mode: DescribeMode;
  labels: boolean;
  reifiers: boolean;
  maxTriples: number | null;
  maxDepth: number | null;
};

/** `GET /$/describe/{ds}`: the options in force, and whether the dataset sets them. */
export type DescribeSetting = DescribeOptions & {
  source: 'default' | 'dataset';
  modes: DescribeMode[];
};

/** `GET /$/compaction/{ds}`: what automatic compaction sees of a dataset, and does. */
export type CompactionStatus = {
  dataset: string;
  /** Effective: the server's switch, the dataset's own and a writable server. */
  enabled: boolean;
  /** False under `--no-auto-compact`. */
  serverEnabled: boolean;
  policy: CompactionPolicy;
  /** The settings the dataset overrides (its `compaction.json`). */
  own: Partial<CompactionPolicy>;
  state: 'off' | 'idle' | 'due' | 'deferred' | 'running';
  trigger?: string;
  triggerKind?: 'max-delta' | 'ratio' | 'delta-bytes' | 'wal-bytes' | 'idle' | 'age';
  /** Why a due compaction waits (`min-interval`, `backup`, `history`, `disk`, …). */
  deferred?: string;
  deferredDetail?: string;
  task?: string;
  measures: {
    generation: string;
    baseQuads: number;
    deltaQuads: number;
    deltaBytes: number;
    walBytes: number;
    idleSeconds: number | null;
    oldestChangeSeconds: number | null;
    /** The delta size at which the relative trigger fires. */
    threshold: number;
  };
  last?: {
    automatic: boolean;
    trigger?: string;
    startedAt: string;
    finishedAt: string;
    seconds: number;
    outcome: 'done' | 'failed' | 'abandoned' | 'cancelled';
    generation?: string;
    lockMs?: number;
    buildMs?: number;
    caughtUpCommits?: number;
    /** `partial` when the compaction rewrote only the blocks its delta touched. */
    mode?: 'full' | 'partial';
    blocksRewritten?: number;
    blocksCopied?: number;
    /** Why a compaction that could have been partial rewrote everything. */
    fullReason?: string;
    error?: string;
  };
  automaticRuns: number;
  failures: number;
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
  predicates: {
    iri: string;
    count: number;
    distinctSubjects: number;
    distinctObjects: number;
  }[];
  classes: { iri: string; instances: number }[];
  diskBytes: number;
  /** Storage quota of a persistent dataset (null in memory); absent on servers that predate it. */
  quota?: DatasetQuota | null;
  /** Decoded-block cache. */
  cache: { entries: number; bytes: number; hits: number; misses: number };
  /** Query (sub)result cache; absent on servers that predate it. */
  resultCache?: {
    enabled: boolean;
    entries: number;
    bytes: number;
    hits: number;
    misses: number;
  };
  /** Reasoning status; absent on servers that predate it. */
  reasoning?: ReasoningStatus | null;
  /** Spatial index status (null: disabled); absent on servers that predate it. */
  geo?: GeoStatus | null;
  /** Automatic compaction; absent on servers that predate it. */
  compaction?: CompactionStatus;
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
  | 'vector-index'
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
  return {
    kind,
    limit: Number(body.limit ?? 0),
    requested: Number(body.requested ?? 0),
  };
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
    throw new ApiError(0, 'Cannot reach the Sparkles server', {
      detail: String(e),
    });
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
    throw new ApiError(res.status, 'Server returned invalid JSON', {
      detail: text.slice(0, 300),
    });
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

let serverInfoCache: { at: number; info: Promise<ServerInfo> } | null = null;

/**
 * `GET /$/server` for callers that need only its settled parts (read-only mode, the map
 * style): one request serves every caller for `maxAgeMs`.
 */
export function cachedServerInfo(maxAgeMs = 60_000): Promise<ServerInfo> {
  if (!serverInfoCache || Date.now() - serverInfoCache.at > maxAgeMs) {
    const info = serverInfo();
    serverInfoCache = { at: Date.now(), info };
    info.catch(() => {
      if (serverInfoCache?.info === info) serverInfoCache = null;
    });
  }
  return serverInfoCache.info;
}

/** Readiness; resolves with the document for `503` (not ready) as well. */
export async function ready(signal?: AbortSignal): Promise<ReadyInfo> {
  let res: Response;
  try {
    res = await fetch('/$/ready', { signal, cache: 'no-store' });
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'Cannot reach the Sparkles server', {
      detail: String(e),
    });
  }
  if (res.status !== 200 && res.status !== 503) throw await toError(res);
  return (await res.json()) as ReadyInfo;
}

/** The metrics registry as JSON (`404` when the server runs with `--no-metrics`). */
export const metricsSnapshot = (signal?: AbortSignal) =>
  json<MetricsSnapshot>('/$/metrics?format=json', {
    signal,
    cache: 'no-store',
  });

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

export type FormatWarning = {
  code: string;
  message: string;
  line: number;
  column: number;
};

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

export type LintSeverity = 'error' | 'warning' | 'info' | 'hint';

/** `POST /$/lint` (and the browser module's `lint`): SPARQL, Turtle or TriG. */
export type LintRequest = {
  text: string;
  language?: 'sparql' | 'turtle' | 'trig';
  /** rule levels over the defaults: a severity or `off` */
  rules?: Record<string, LintSeverity | 'off'>;
  /** apply the safe fixes and return the fixed `text` */
  fix?: boolean;
};

export type LintEdit = { from: number; to: number; insert: string };

export type LintDiagnostic = {
  rule: string;
  severity: LintSeverity;
  message: string;
  line: number;
  column: number;
  endLine: number;
  endColumn: number;
  /** the range in UTF-16 code units (the editor's unit) */
  from: number;
  to: number;
  /** a safe fix */
  fix?: { title: string; edits: LintEdit[] };
};

export type LintResult = {
  language: string;
  diagnostics: LintDiagnostic[];
  /** with `fix`: the fixed text and the number of fixes applied */
  text?: string;
  applied?: number;
};

/**
 * Lint a document. A syntax error is a finding (`rule: 'syntax'`), not an error. Throws an
 * `ApiError`: `400` `bad-request`, `415` for a language lint does not take, `404` when the
 * server turned formatting off.
 */
export const lint = (req: LintRequest, signal?: AbortSignal) =>
  json<LintResult>('/$/lint', { ...jsonBody(req), signal });

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

/** `GET /$/compaction/{ds}`. */
export const compactionStatus = (ds: string) => json<CompactionStatus>(`/$/compaction/${enc(ds)}`);

/** Replace the dataset's own compaction settings (`PUT /$/compaction/{ds}`). */
export const setCompaction = (ds: string, own: Partial<CompactionPolicy>) =>
  json<CompactionStatus>(`/$/compaction/${enc(ds)}`, {
    ...jsonBody(own),
    method: 'PUT',
  });

/** Remove the dataset's own compaction settings (`DELETE /$/compaction/{ds}`). */
export const clearCompaction = (ds: string) =>
  json<CompactionStatus>(`/$/compaction/${enc(ds)}`, { method: 'DELETE' });

/** `GET /$/describe/{ds}`. */
export const describeSetting = (ds: string) =>
  json<DescribeSetting>(`/$/describe/${enc(ds)}`, { cache: 'no-store' });

/** Replace the dataset's DESCRIBE setting (`PUT /$/describe/{ds}`). */
export const setDescribeSetting = (ds: string, o: DescribeOptions) =>
  json<DescribeSetting>(`/$/describe/${enc(ds)}`, { ...jsonBody(o), method: 'PUT' });

/** Back to the server's defaults (`DELETE /$/describe/{ds}`). */
export const clearDescribeSetting = (ds: string) =>
  json<DescribeSetting>(`/$/describe/${enc(ds)}`, { method: 'DELETE' });

export const backup = (ds: string) => json<Task>(`/$/backup/${enc(ds)}`, { method: 'POST' });

export const reason = (ds: string, profile: ReasonProfile, rules?: string) =>
  json<Task>(
    `/$/reason/${enc(ds)}`,
    jsonBody(profile === 'rules' ? { profile, rules } : { profile }),
  );

/** Drop the dataset's cached query results (`POST /$/cache/clear/{ds}`, a Sparkles extension). */
export const clearResultCache = (ds: string) =>
  json<{ cleared: number; bytes: number }>(`/$/cache/clear/${enc(ds)}`, {
    method: 'POST',
  });

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
    /** Anonymous superclasses in the OWL 2 Manchester Syntax, IRIs in angle brackets. */
    superClassExpressions?: string[];
    /** Anonymous equivalent classes, rendered the same way. */
    equivalentClassExpressions?: string[];
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
    /** With `detail: ['subjectClasses']`: the classes of the subjects, by class IRI. */
    subjectClasses?: { class: string; triples: number; subjects: number }[];
    /** With `detail: ['subjectClasses']`: subjects without an IRI class. */
    untypedSubjects?: { triples: number; subjects: number };
  };
  declared: {
    types: string[];
    domains: string[];
    ranges: string[];
    superProperties: string[];
    inverseOf: string[];
    /** Anonymous domains and ranges in the OWL 2 Manchester Syntax. */
    domainExpressions?: string[];
    rangeExpressions?: string[];
    labels: Lit[];
    comments: Lit[];
  };
};

export type Page<T> = { items: T[]; total: number; next: string | null };

/**
 * What checks a SHACL constraint: write-time validation that refuses a write breaking
 * it, write-time validation that commits the write and reports it, or nothing until
 * the data is validated on request.
 */
export type Enforcement = 'reject-on-write' | 'warn-on-write' | 'validated-on-request';

/** One property shape with a predicate path. */
export type PropertyConstraint = {
  path: string;
  shape?: string;
  severity: string;
  enforcement: Enforcement;
  minCount?: number;
  maxCount?: number;
  datatype?: string;
  class?: string[];
  nodeKind?: string;
  /** Constraint components of the shape's other constraints. */
  other?: string[];
};

export type ClassConstraints = {
  class: string;
  shapes: string[];
  closed: boolean;
  properties: PropertyConstraint[];
  /** Property shapes whose path is not a single predicate (not listed). */
  otherPaths: number;
};

export type ConstraintSource = {
  /** `guard`: the write-time validation's shapes; `graphs`: shapes graphs named by the request. */
  kind: 'guard' | 'graphs';
  graphs: string[];
  file?: boolean;
  mode?: string;
  threshold?: string;
  shapes: number;
  otherTargets: number;
  classes: ClassConstraints[];
};

/** The SHACL constraints layer of the schema report: declared, never observed. */
export type ConstraintsLayer = { sources: ConstraintSource[] };

/** The state a schema report was computed at. */
export type SchemaSnapshot = {
  version: number;
  /** The commit (reports since per-commit maintenance). */
  commit?: number;
  generation: string;
  computedAt: string;
};

export type SchemaSummary = {
  schemaFormat: 1;
  dataset: string;
  snapshot: SchemaSnapshot;
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
  ontology: {
    iri: string;
    labels: Lit[];
    versionInfo: Lit[];
    comments: Lit[];
  }[];
  hierarchy: { roots: string[]; cycles: string[][] };
  classes: Page<SchemaClass>;
  predicates: Page<SchemaPredicate>;
  /** The write-time validation's SHACL shapes by default, or the sources of `shapes`. */
  constraints?: ConstraintsLayer;
};

export type SchemaOptions = {
  /** `default`, `union` or a graph IRI. */
  graph?: string;
  /** Graph read for declarations (server default: the same as `graph`). */
  declaredGraph?: string;
  /** Count materialized inferences (server default: yes, when present). */
  reasoning?: boolean;
  declared?: 'asserted' | 'all';
  /** Shapes of the constraints layer: `guard`, `default`, `none` or graph IRIs. */
  shapes?: string[];
  /** Extra per-predicate details, at extra cost. */
  detail?: 'subjectClasses'[];
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
  for (const s of opts.shapes ?? []) p.append('shapes', s);
  if (opts.detail?.length) p.set('detail', opts.detail.join(','));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.timeout != null) p.set('timeout', String(opts.timeout));
  if (cursor) p.set('cursor', cursor);
  const s = p.toString();
  return s ? `?${s}` : '';
}

/** `GET /$/schema/{ds}`: the report with the first page of classes and predicates. */
export const schemaSummary = (ds: string, opts: SchemaOptions = {}) =>
  json<SchemaSummary>(`/$/schema/${enc(ds)}${schemaParams(opts)}`, {
    signal: opts.signal,
  });

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
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: SPARKLES_JSON,
    },
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
    headers: {
      'Content-Type': 'application/sparql-update',
      Accept: 'application/json',
    },
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
  /** The syntax of the shapes: Turtle (the default) or SHACLC. */
  syntax?: 'turtle' | 'shaclc';
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
    headers: {
      'Content-Type': opts.syntax === 'shaclc' ? 'text/shaclc' : 'text/turtle',
      Accept: accept,
    },
    body: shapes,
    signal: opts.signal,
  });
}

/** Validate a data graph against a shapes graph (Turtle or SHACLC); compact JSON report. */
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
  | {
      kind: 'nodeKind' | 'datatype' | 'facet' | 'valueSet';
      value: Term;
      constraint: string;
    }
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
    headers: {
      'Content-Type': 'application/json',
      Accept: 'application/json, text/plain',
    },
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
  /** The CSV and TSV tables mapped to triples, one report each. */
  tables?: {
    file: string;
    rows: number;
    triples: number;
    warnings?: string[];
  }[];
  /** The commit the upload produced (absent on servers that predate commits). */
  receipt?: Receipt;
};

/**
 * Multipart upload to /{ds}/upload with progress reporting (XHR, since fetch has no upload
 * progress). Asks for a commit receipt (`receipt=true`). `tables` maps the CSV and TSV
 * files: `base` and `key` go in the query string, and a mapping or template file in the
 * part the server reads it from.
 */
export async function upload(
  ds: string,
  files: File[],
  opts: {
    graph?: string;
    tables?: { base?: string; key?: string; mapping?: File | null };
    onProgress?: (p: UploadProgress) => void;
    signal?: AbortSignal;
  } = {},
): Promise<UploadResult | string> {
  const csrf = await csrfFor('POST');
  return new Promise((resolve, reject) => {
    const form = new FormData();
    if (opts.graph) form.append('graph', opts.graph);
    const mapping = opts.tables?.mapping;
    if (mapping) form.append(mappingPart(mapping.name), mapping, mapping.name);
    for (const f of files) form.append('file', f, f.name);
    const params = opts.tables ? tableParams(opts.tables) : '';
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `/${enc(ds)}/upload?receipt=true${params ? `&${params}` : ''}`);
    xhr.setRequestHeader('Accept', 'application/json');
    if (csrf) xhr.setRequestHeader(CSRF_HEADER, csrf);
    xhr.upload.onprogress = (e) =>
      opts.onProgress?.({
        loaded: e.loaded,
        total: e.lengthComputable ? e.total : 0,
      });
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
      const b = body as {
        error?: string;
        detail?: string;
        line?: number;
        column?: number;
      };
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
export type InferencesNotice = {
  stale: boolean | null;
  commitsSince: number | null;
};

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
  source: {
    name: string;
    path?: string;
    version: number;
    generation: string;
    quads: number;
  };
  forkedFrom: { id: string; seq: number };
  inferences: 'copy' | 'drop';
  /** A partial clone's selection. */
  graphs?: string[];
  /** How the index was made; absent on servers that predate it. */
  method?: CloneMethod;
};

/** Copy one snapshot of `ds` into the new dataset `name` (a task); see `cloneBody`. */
export const cloneDataset = (ds: string, name: string, opts: CloneOptions = {}) =>
  json<Task>(`/$/datasets/${enc(ds)}/clone`, jsonBody(cloneBody(name, opts)));

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
  | 'embed'
  | 'patch'
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
  /** The write skipped the dataset's write-time validation (a bypass). */
  unvalidated?: boolean;
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
  skipped: {
    malformed: number;
    unknownCrs: number;
    tooLarge: number;
    empty: number;
  };
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
  properties: {
    subject: string;
    feature?: string;
    graph: string | null;
    predicate: string;
  };
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

// --- vector indexes ----------------------------------------------------------------

export type VectorMetric = 'cosine' | 'dot' | 'euclidean';
export type VectorIndexState = 'ready' | 'building' | 'failed' | 'over-budget';

/** The HNSW settings of an index (the defaults are 16, 128 and 128). */
export type HnswConfig = {
  m?: number;
  efConstruction?: number;
  efSearch?: number;
};

/** The body of `PUT /$/vector/{ds}/{name}`. */
export type VectorIndexConfig = {
  predicate: string;
  dimension: number;
  /** Default cosine. */
  metric?: VectorMetric;
  /** A label of the embedding model, not interpreted. */
  model?: string;
  /** `false` keeps only the packed vectors, which are searched exactly. */
  hnsw?: HnswConfig | false;
  /** Searches over at most this many rows are exact; default 10000. */
  exactThreshold?: number;
  /** Compute the vectors with an OpenAI-compatible embeddings endpoint. */
  embedding?: EmbeddingConfig;
};

/** Where an embedding request's bearer token comes from (the API accepts `secret`). */
export type EmbeddingApiKey = { secret: string } | { env: string } | { file: string };

/** `VectorIndexConfig.embedding`: which literals are embedded, and by which endpoint. */
export type EmbeddingConfig = {
  url: string;
  model: string;
  apiKey?: EmbeddingApiKey;
  sendDimensions?: boolean;
  predicates?: string[];
  languages?: string[];
  classes?: string[];
  query?: string;
  combine?: boolean;
  inputPrefix?: string;
  queryPrefix?: string;
  queryText?: boolean;
  batchSize?: number;
  maxInputChars?: number;
  chunking?: { size: number; overlap?: number; unit?: 'chars' | 'tokens' };
  requestsPerMinute?: number;
  tokensPerMinute?: number;
  maxRetries?: number;
  timeoutSecs?: number;
};

export type EmbeddingState = 'idle' | 'scanning' | 'embedding' | 'backoff' | 'paused' | 'disabled';

/** The embedding worker of an index that computes its vectors. */
export type EmbeddingStatus = {
  state: EmbeddingState;
  model: string;
  /** The endpoint's URL without credentials or query. */
  endpoint: string;
  /** Subjects (per graph) waiting to be embedded. */
  backlog: number;
  /** A full pass in progress. */
  scan?: { done: number; total: number };
  /** Every commit up to this one has its text embedded. */
  appliedSeq: number;
  headSeq: number;
  /** Since the store was opened. */
  embedded: number;
  requests: number;
  failed: number;
  lastError?: { at: string; message: string; subject?: string };
  /** When the worker tries again, in backoff. */
  retryAt?: string;
  lastBatch?: { at: string; inputs: number; ms: number };
  /** The index's embedding configuration (it names secrets, never holds keys). */
  config: EmbeddingConfig;
};

export type VectorIndexStatus = {
  name: string;
  predicate: string;
  dimension: number;
  metric: VectorMetric;
  model?: string;
  state: VectorIndexState;
  /** Build progress (0–1) while building. */
  progress?: number;
  message?: string;
  /** The generation the index was built for. */
  generation: string;
  /** Vectors in the generation's base (the packed rows). */
  rows: number;
  /** Changes since the base that searches add exactly. */
  overlay: { inserts: number; deletes: number };
  skipped: { malformed: number; wrongDimension: number; zeroNorm: number };
  /** `residency`: `heap` (built in this process) or `mmap` (read in place from the file). */
  memory: { segmentBytes: number; hnswBytes: number; residency: string };
  /** Null for an index that is searched exactly. */
  hnsw: {
    m: number;
    efConstruction: number;
    efSearch: number;
    nodes: number;
    layers: number;
  } | null;
  exactThreshold: number;
  /** The index file of a persistent store; `opened`: read from it, not built. */
  files?: { bytes: number; opened: boolean };
  lastBuild?: { at: string; ms: number; rows: number };
  /** The embedding worker, for an index that computes its vectors. */
  embedding?: EmbeddingStatus;
};

/** `GET /$/vector/{ds}`. */
export type VectorStatus = {
  budgetBytes: number;
  usedBytes: number;
  generation: string;
  indexes: VectorIndexStatus[];
  /** Predicates packed without an index (on their first search). */
  predicates: {
    predicate: string;
    bytes: number;
    malformed: number;
    dimensions: { dimension: number; vectors: number }[];
  }[];
};

/** `POST /$/vector/{ds}/{name}/recall`: recall@k of the graph against the exact search. */
export type VectorRecall = {
  k: number;
  samples: number;
  ef: number;
  /** 0–1. */
  recall: number;
  /** Mean milliseconds per search. */
  hnswMs: number;
  exactMs: number;
};

const vectorPath = (ds: string, name?: string) =>
  `/$/vector/${enc(ds)}${name == null ? '' : `/${enc(name)}`}`;

/**
 * The vector indexes and packed predicates of a dataset. Servers that predate vector
 * indexes answer 404.
 */
export const vectorStatus = (ds: string, signal?: AbortSignal) =>
  json<VectorStatus>(vectorPath(ds), { signal, cache: 'no-store' });

/** Create or replace an index. Its build is the returned task, and `409` means another index has the predicate. */
export const putVectorIndex = (ds: string, name: string, config: VectorIndexConfig) =>
  json<{ index: VectorIndexStatus; task: Task }>(vectorPath(ds, name), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(config),
  });

/** Drop an index and its files. */
export const dropVectorIndex = (ds: string, name: string) =>
  json<unknown>(vectorPath(ds, name), { method: 'DELETE' });

/** Build an index again from RDF. */
export const rebuildVectorIndex = (ds: string, name: string) =>
  json<Task>(`${vectorPath(ds, name)}/rebuild`, { method: 'POST' });

/** Embed every selected text of an index again (after its model changed). */
export const reembedVectorIndex = (ds: string, name: string) =>
  json<VectorIndexStatus>(`${vectorPath(ds, name)}/reembed`, {
    method: 'POST',
  });

/** Measure recall@k against the exact search, with stored vectors as the queries. */
export function vectorRecall(
  ds: string,
  name: string,
  opts: { samples?: number; k?: number; ef?: number } = {},
  signal?: AbortSignal,
) {
  const p = new URLSearchParams();
  if (opts.samples != null) p.set('samples', String(opts.samples));
  if (opts.k != null) p.set('k', String(opts.k));
  if (opts.ef != null) p.set('ef', String(opts.ef));
  const q = p.toString();
  return json<VectorRecall>(`${vectorPath(ds, name)}/recall${q ? `?${q}` : ''}`, {
    method: 'POST',
    signal,
  });
}

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
  return json<Diff>(`/${enc(ds)}/diff${qs ? `?${qs}` : ''}`, {
    signal: opts.signal,
  });
}

/** One recorded change of `GET /{ds}/history`. */
export type HistoryChange = {
  op: 'add' | 'remove';
  subject: string;
  predicate: string;
  object: string;
  /** null for the default graph */
  graph: string | null;
  commit: number;
  timestamp: string;
  kind: string;
  author?: string;
  message?: string;
};

/** `GET /{ds}/history`: the recorded changes of a range of commits. */
export type HistoryChanges = {
  dataset: string;
  datasetId: string;
  head: number;
  from: number;
  to: number;
  truncated: boolean;
  changes: HistoryChange[];
  /** commits whose changes the change log does not hold */
  unrecorded: {
    from: number;
    to: number;
    reason: 'before-log' | 'bulk' | 'gap';
  }[];
};

export type HistoryOptions = {
  /** N-Triples terms or bare IRIs */
  subject?: string;
  predicate?: string;
  object?: string;
  /** an IRI, or `default` */
  graph?: string;
  from?: string;
  to?: string;
  op?: 'add' | 'remove';
  order?: 'asc' | 'desc';
  limit?: number;
  signal?: AbortSignal;
};

export function historyChanges(ds: string, opts: HistoryOptions = {}): Promise<HistoryChanges> {
  const p = new URLSearchParams();
  for (const k of [
    'subject',
    'predicate',
    'object',
    'graph',
    'from',
    'to',
    'op',
    'order',
  ] as const) {
    const v = opts[k];
    if (v) p.set(k, v);
  }
  if (opts.limit != null) p.set('limit', String(opts.limit));
  const qs = p.toString();
  return json<HistoryChanges>(`/${enc(ds)}/history${qs ? `?${qs}` : ''}`, {
    signal: opts.signal,
    cache: 'no-store',
  });
}

// ------------------------------------------------------ write-time validation ------

export type GuardStatusName = 'passed' | 'warned' | 'rejected' | 'skipped' | 'bypassed';

/** One validated write (`lastCheck`, `recentRejections`). */
export type ValidationCheck = {
  time: string;
  kind: string;
  status: GuardStatusName;
  strategy: 'full' | 'incremental' | 'none';
  blocking: number;
  /** grandfather mode: the blocking results the write introduced */
  introduced?: number;
  total: number;
  focusNodes?: number;
  /** why the write, or some shape, was validated in full */
  fallback?: string;
  millis: number;
  /** the first result: a SHACL result or a ShEx result-map entry, as JSON */
  first?: Record<string, unknown>;
};

/** The validation state of the head (`null` conforms: unknown). */
export type ValidationBaseline = {
  commit: number;
  conforms: boolean | null;
  blocking: number;
  total: number;
  bySeverity: { violation: number; warning: number; info: number };
  millis: number;
};

export type WriteValidationStatus = {
  mode: 'reject' | 'warn' | 'off';
  shapeCount: number;
  /** ShEx: the associations of the shape map at the last validation */
  associations?: number | null;
  baseline: ValidationBaseline | null;
  lastFullMillis: number | null;
  counters: Record<GuardStatusName, number>;
  warnings: string[];
  /** SHACL: how shapes are validated on a write */
  incremental?: {
    localShapes: number;
    fullShapes: { shape: string; reason: string }[];
  };
  lastCheck?: ValidationCheck | null;
  recentRejections?: ValidationCheck[];
};

export type WriteValidation = {
  language: 'shacl' | 'shex';
  config: {
    mode: 'reject' | 'warn' | 'off';
    threshold?: 'violation' | 'warning' | 'info';
    baseline?: 'strict' | 'grandfather';
    includeInferences?: boolean;
    dataGraph?: string | string[];
    shapes?: { graphs?: string[]; file?: string };
    timeoutSeconds?: number;
    updated?: string;
  };
  status: WriteValidationStatus;
};

/**
 * `GET /$/validation/{ds}`: the dataset's write-time validation and its status, or null
 * when validation is off.
 */
export async function writeValidation(
  ds: string,
  signal?: AbortSignal,
): Promise<WriteValidation | null> {
  const body = await json<WriteValidation | { config: null }>(`/$/validation/${enc(ds)}`, {
    signal,
    cache: 'no-store',
  });
  return body && body.config ? (body as WriteValidation) : null;
}

/**
 * `PUT /$/validation/{ds}` in warn mode with inline shapes (SHACL Turtle) or an inline ShEx
 * schema and its shape map. It replaces any configuration the dataset has; needs admin.
 */
export function installWarnGuard(
  ds: string,
  g: { language: 'shacl'; shapes: string } | { language: 'shex'; schema: string; shapeMap: string },
): Promise<unknown> {
  const body =
    g.language === 'shacl'
      ? { language: 'shacl', mode: 'warn', shapes: { inline: g.shapes } }
      : {
          language: 'shex',
          mode: 'warn',
          schema: { inline: g.schema, format: 'shexc' },
          shapeMap: g.shapeMap,
        };
  return json(`/$/validation/${enc(ds)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

// ------------------------------------------------------------ drafted shapes ------

/** One constraint of a drafted property shape, with what it would exclude. */
export type DraftConstraint = {
  component: string;
  value: number | string | string[] | boolean;
  applicable: number;
  satisfied: number;
  excluded: number;
};

export type DraftProperty = {
  path: string;
  instances: number;
  maxValues: number;
  constraints: DraftConstraint[];
  rejected: DraftConstraint[];
};

export type DraftShape = {
  shape: string;
  class: string;
  instances: number;
  closed: boolean;
  properties: DraftProperty[];
};

/** `GET /$/schema/{ds}/shapes` (JSON): shapes drafted from the data. */
export type ShapesDraft = {
  draftFormat: number;
  dataset: string;
  snapshot: { version: number; generation: string; computedAt: string };
  selection: { graph: string; reasoning: boolean };
  options: {
    support: number;
    minInstances: number;
    maxIn: number;
    maxCount: number;
    closed: boolean;
    base: string;
    classes: string[];
  };
  totals: {
    shapes: number;
    propertyShapes: number;
    constraints: number;
    rejected: number;
    skippedClasses: number;
  };
  shapes: DraftShape[];
  /** The shapes graph in Turtle. */
  shacl: string;
  /** The shapes graph in the SHACL Compact Syntax. */
  shaclc: string;
  /** The ShEx schema in ShExC. */
  shex: string;
  /** The query shape map of the ShEx schema. */
  shapeMap: string;
};

export type DraftOptions = {
  support?: number;
  closed?: boolean;
  graph?: string;
  reasoning?: boolean;
  classes?: string[];
  signal?: AbortSignal;
};

/** The URL of a draft request. */
export function draftShapesPath(ds: string, opts: DraftOptions = {}): string {
  const p = new URLSearchParams();
  if (opts.support != null && opts.support !== 1) p.set('support', String(opts.support));
  if (opts.closed) p.set('closed', 'true');
  if (opts.graph && opts.graph !== 'default') p.set('graph', opts.graph);
  if (opts.reasoning) p.set('reasoning', 'true');
  for (const c of opts.classes ?? []) p.append('class', c);
  const s = p.toString();
  return `/$/schema/${enc(ds)}/shapes${s ? `?${s}` : ''}`;
}

export const draftShapes = (ds: string, opts: DraftOptions = {}) =>
  json<ShapesDraft>(draftShapesPath(ds, opts), {
    signal: opts.signal,
    cache: 'no-store',
  });

// ------------------------------------------------- class profiles, schema diffs ------

/** One predicate the instances of a class use. */
export type ProfileProperty = {
  predicate: string;
  /** Instances with at least one value. */
  instances: number;
  triples: number;
  minPerInstance: number;
  maxPerInstance: number;
  objects: {
    iri?: number;
    blank?: number;
    tripleTerm?: number;
    literals?: { datatype: string; triples: number }[];
  };
  /** The classes of the IRI and blank-node values, most triples first. */
  objectClasses: { class: string; triples: number }[];
};

/** The profile of one class: the predicates its instances use, and those that point at them. */
export type ClassProfile = {
  class: string;
  builtin: boolean;
  instances: number;
  properties: ProfileProperty[];
  incoming: { predicate: string; triples: number; instances: number }[];
};

/** `GET /$/schema/{ds}/profiles`. */
export type ClassProfiles = {
  profileFormat: number;
  snapshot: SchemaSnapshot;
  selection: { graph: string; reasoning: boolean };
  classes: ClassProfile[];
};

export type ProfileOptions = {
  graph?: string;
  reasoning?: boolean;
  classes?: string[];
  signal?: AbortSignal;
};

/** The URL of a profiles request. */
export function schemaProfilesPath(ds: string, opts: ProfileOptions = {}): string {
  const p = new URLSearchParams();
  if (opts.graph && opts.graph !== 'default') p.set('graph', opts.graph);
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  for (const c of opts.classes ?? []) p.append('class', c);
  const s = p.toString();
  return `/$/schema/${enc(ds)}/profiles${s ? `?${s}` : ''}`;
}

export const schemaProfiles = (ds: string, opts: ProfileOptions = {}) =>
  json<ClassProfiles>(schemaProfilesPath(ds, opts), {
    signal: opts.signal,
    cache: 'no-store',
  });

/** One difference of a field between two reports: a value, or members of a list. */
export type SchemaChange =
  | { path: string; from: unknown; to: unknown }
  | { path: string; added: unknown[]; removed: unknown[] };

export type SchemaEntryChanges<T> = {
  added: T[];
  removed: T[];
  changed: { iri: string; changes: SchemaChange[] }[];
};

/** `GET /$/schema/{ds}/diff`: what changed from the report of one state to another's. */
export type SchemaDiff = {
  diffFormat: number;
  from: SchemaSnapshot;
  to: SchemaSnapshot;
  selection: SchemaSummary['selection'];
  counts: {
    classesAdded: number;
    classesRemoved: number;
    classesChanged: number;
    predicatesAdded: number;
    predicatesRemoved: number;
    predicatesChanged: number;
  };
  /** Changes of the totals, the hierarchy and the ontology headers. */
  report: SchemaChange[];
  classes: SchemaEntryChanges<SchemaClass>;
  predicates: SchemaEntryChanges<SchemaPredicate>;
};

export type SchemaDiffOptions = {
  /** The earlier state: `42`, `commit:42`, `time:<RFC 3339>` or `snapshot:NAME`. */
  from: string;
  /** The later state (default: the head). */
  to?: string;
  graph?: string;
  reasoning?: boolean;
  signal?: AbortSignal;
};

/** The URL of a schema diff request. */
export function schemaDiffPath(ds: string, opts: SchemaDiffOptions): string {
  const p = new URLSearchParams();
  p.set('from', opts.from);
  if (opts.to) p.set('to', opts.to);
  if (opts.graph && opts.graph !== 'default') p.set('graph', opts.graph);
  if (opts.reasoning != null) p.set('reasoning', String(opts.reasoning));
  return `/$/schema/${enc(ds)}/diff?${p.toString()}`;
}

export const schemaDiff = (ds: string, opts: SchemaDiffOptions) =>
  json<SchemaDiff>(schemaDiffPath(ds, opts), {
    signal: opts.signal,
    cache: 'no-store',
  });

// ------------------------------------------------------------ stored queries ------

export type StoredParamType =
  | 'iri'
  | 'string'
  | 'integer'
  | 'decimal'
  | 'double'
  | 'boolean'
  | 'date'
  | 'dateTime'
  | 'literal'
  | 'term';

export type StoredParam = {
  type: StoredParamType;
  description?: string;
  default?: string | number | boolean;
  required?: boolean;
  datatype?: string;
  language?: string;
  enum?: (string | number | boolean)[];
};

/** The definition a client stores (`PUT /$/queries/{ds}/{name}`). */
export type StoredDefinition = {
  query: string;
  description?: string;
  parameters?: Record<string, StoredParam>;
  results?: string;
  mcp?: boolean;
};

export type StoredVersion = {
  version: number;
  parent?: number;
  created: string;
  author?: string;
  message?: string;
  datasetCommit?: number;
  digest: string;
};

/** A stored query as `GET /$/queries/{ds}/{name}` answers (listings leave out `query`). */
export type StoredQuery = Omit<StoredDefinition, 'query'> & {
  query?: string;
  name: string;
  kind?: QueryType;
  mcp: boolean;
  version: StoredVersion;
};

export async function storedQueries(ds: string, signal?: AbortSignal): Promise<StoredQuery[]> {
  const body = await json<{ queries: StoredQuery[] }>(`/$/queries/${enc(ds)}`, {
    signal,
    cache: 'no-store',
  });
  return body?.queries ?? [];
}

export const storedQuery = (ds: string, name: string) =>
  json<StoredQuery>(`/$/queries/${enc(ds)}/${enc(name)}`, {
    cache: 'no-store',
  });

export const putStoredQuery = (ds: string, name: string, def: StoredDefinition) =>
  json<StoredQuery & { changed: boolean }>(`/$/queries/${enc(ds)}/${enc(name)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(def),
  });

export async function deleteStoredQuery(ds: string, name: string): Promise<void> {
  await request(`/$/queries/${enc(ds)}/${enc(name)}`, { method: 'DELETE' });
}

/**
 * Run a stored query with parameter values (a form body) and get the rich UI result
 * format, as `query` does.
 */
export async function runStoredQuery(
  ds: string,
  name: string,
  values: Record<string, string>,
  opts: QueryOptions = {},
): Promise<SparklesResult> {
  const res = await request(`/${enc(ds)}/queries/${enc(name)}${queryParams(opts)}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/x-www-form-urlencoded',
      Accept: SPARKLES_JSON,
    },
    body: new URLSearchParams(values).toString(),
    signal: opts.signal,
  });
  const body = (await res.json()) as SparklesResult;
  const inferences = parseInferencesHeader(res.headers.get('Sparkles-Inferences'));
  if (inferences) body.inferences = inferences;
  const at = atInfo(res.headers);
  if (at) body.at = at;
  return normalizeResult(body);
}
