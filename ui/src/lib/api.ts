// Typed client for the Sparkles HTTP API (see docs/API.md — the source of truth).
// All paths are absolute from the server root; the UI itself lives under /ui/.

export type DatasetType = 'persistent' | 'mem';

export type DatasetInfo = {
  name: string;
  type: DatasetType;
  endpoints: { query: string; update: string; gsp: string; upload: string; shacl?: string };
  quads: number;
  reasoning: null | { profile: string; inferred: number; at: string };
};

export type ServerInfo = {
  version: string;
  startedAt: string;
  uptimeSeconds: number;
  datasets: DatasetInfo[];
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
};

export type TaskKind = 'compact' | 'backup' | 'reason' | 'load';
export type Task = {
  id: string;
  kind: TaskKind;
  dataset: string;
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
