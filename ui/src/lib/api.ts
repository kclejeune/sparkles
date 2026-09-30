// Typed client for the Sparkles HTTP API (see docs/API.md — the source of truth).
// All paths are absolute from the server root; the UI itself lives under /ui/.

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
};

export type ServerInfo = {
  version: string;
  startedAt: string;
  uptimeSeconds: number;
  datasets: DatasetInfo[];
  /** Absent on servers that predate it. */
  readOnly?: boolean;
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

export type TaskKind = 'compact' | 'backup' | 'reason' | 'load' | 'clone';
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
  meta: { totalRows: number; sentRows: number; timing: Timing; plan: PlanNode };
  /** From the `Sparkles-Inferences` header: the result used outdated inferences. */
  inferences?: InferencesNotice;
};

export type ExplainResult = { algebra: string; plan: PlanNode };

export type ReasonProfile = 'rdfs' | 'owl-rl' | 'rules';

/** Error shape for non-2xx responses: `{ error, detail?, line?, column? }`. */
export class ApiError extends Error {
  status: number;
  detail?: string;
  line?: number;
  column?: number;
  constructor(
    status: number,
    message: string,
    extra: { detail?: string; line?: number; column?: number } = {},
  ) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.detail = extra.detail;
    this.line = extra.line;
    this.column = extra.column;
  }
}

export const SPARKLES_JSON = 'application/x-sparkles+json';

const enc = encodeURIComponent;

async function toError(res: Response): Promise<ApiError> {
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
      });
    }
  } catch {
    /* not JSON */
  }
  const fallback = text.trim().slice(0, 500) || res.statusText || 'Request failed';
  return new ApiError(res.status, `${res.status} ${fallback}`);
}

async function request(path: string, init: RequestInit = {}): Promise<Response> {
  let res: Response;
  try {
    res = await fetch(path, init);
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'Cannot reach the Sparkles server', { detail: String(e) });
  }
  if (!res.ok) throw await toError(res);
  return res;
}

async function json<T>(path: string, init: RequestInit = {}): Promise<T> {
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
};

export async function update(
  ds: string,
  sparql: string,
  signal?: AbortSignal,
): Promise<UpdateResult | null> {
  const res = await request(`/${enc(ds)}/update`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/sparql-update', Accept: 'application/json' },
    body: sparql,
    signal,
  });
  // Fuseki answers with an HTML page; Sparkles with JSON counts.
  const text = await res.text();
  try {
    const body = JSON.parse(text);
    return body && typeof body.inserted === 'number' ? (body as UpdateResult) : null;
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

/** Multipart upload to /{ds}/upload with progress reporting (XHR, since fetch has no upload progress). */
export function upload(
  ds: string,
  files: File[],
  opts: { graph?: string; onProgress?: (p: UploadProgress) => void; signal?: AbortSignal } = {},
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const form = new FormData();
    if (opts.graph) form.append('graph', opts.graph);
    for (const f of files) form.append('file', f, f.name);
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `/${enc(ds)}/upload`);
    xhr.setRequestHeader('Accept', 'application/json');
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
      if (xhr.status >= 200 && xhr.status < 300) return resolve(body);
      const b = body as { error?: string; detail?: string; line?: number; column?: number };
      reject(
        new ApiError(xhr.status, b?.error ?? `Upload failed (${xhr.status})`, {
          detail: b?.detail,
          line: b?.line,
          column: b?.column,
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
