// Vector literals (`urn:x-sparkles:vector`) and similarity search (`spk:vectorSearch`),
// docs/API.md "Vector similarity".
import * as api from './api';
import type { Term } from './api';
import { fmtBytes } from './format';
import {
  abbreviateVector,
  displayIri,
  isVectorLiteral,
  literalText,
  parseVector,
  sparqlIri,
  SPK,
  toSparql,
  VECTOR_B64_DATATYPE,
  VECTOR_DATATYPE,
} from './rdf';

// The literal helpers live in rdf.ts (term display uses them); re-exported here.
export {
  abbreviateVector,
  isVectorLiteral,
  literalText,
  parseVector,
  SPK,
  VECTOR_B64_DATATYPE,
  VECTOR_DATATYPE,
};

export type Metric = 'cosine' | 'dot' | 'euclidean';
export const METRICS: Metric[] = ['cosine', 'dot', 'euclidean'];

type Literal = Extract<Term, { type: 'literal' }>;

/** Predicates of an entity that have vector values, with how many and which dimensions. */
export type VectorPredicate = { iri: string; vectors: Literal[]; dimensions: number[] };

export function vectorPredicates(props: { p: string; o: Term }[]): VectorPredicate[] {
  const by = new Map<string, Literal[]>();
  for (const { p, o } of props) if (isVectorLiteral(o)) by.set(p, [...(by.get(p) ?? []), o]);
  return [...by]
    .map(([iri, vectors]) => ({
      iri,
      vectors,
      dimensions: [
        ...new Set(
          vectors.map((v) => parseVector(v.value, v.datatype)?.length).filter((n) => n != null),
        ),
      ] as number[],
    }))
    .sort((a, b) => a.iri.localeCompare(b.iri));
}

/** Whether higher or lower scores are closer, and what the score is called. */
export function metricInfo(m: Metric): {
  label: string;
  score: string;
  better: 'higher' | 'lower';
} {
  switch (m) {
    case 'cosine':
      return { label: 'Cosine', score: 'similarity', better: 'higher' };
    case 'dot':
      return { label: 'Dot product', score: 'dot product', better: 'higher' };
    case 'euclidean':
      return { label: 'Euclidean', score: 'distance', better: 'lower' };
  }
}

export type SimilarQuery = {
  predicate: string;
  /** The entity whose vector is the query, or a vector literal to use instead. */
  query: { entity: string } | { vector: Literal };
  k: number;
  metric: Metric;
};

/**
 * `spk:vectorSearch` for the k entities closest to the query. One more hit than `k` is
 * asked for, since an entity's own vector is its closest match and gets filtered out.
 */
export function buildSimilarQuery(q: SimilarQuery): string {
  const k = Math.min(Math.max(1, Math.floor(q.k)) + 1, 10000);
  const query = 'entity' in q.query ? sparqlIri(q.query.entity) : toSparql(q.query.vector);
  const order = metricInfo(q.metric).better === 'lower' ? 'ASC(?score)' : 'DESC(?score)';
  return `PREFIX spk: <${SPK}>
SELECT ?s ?score WHERE {
  (?s ?score) spk:vectorSearch (${sparqlIri(q.predicate)} ${query} ${k} "metric:${q.metric}") .
}
ORDER BY ${order}`;
}

export type SimilarHit = { term: Term; score: number };

/**
 * The hits other than `self`, best first, at most `k`. Order is the server's (best first
 * for the metric); the entity itself is dropped wherever it appears.
 */
export function similarHits(
  rows: Record<string, Term | undefined>[],
  self: string,
  k: number,
): SimilarHit[] {
  const out: SimilarHit[] = [];
  for (const r of rows) {
    if (!r.s || (r.s.type === 'uri' && r.s.value === self)) continue;
    out.push({ term: r.s, score: r.score?.type === 'literal' ? Number(r.score.value) : NaN });
    if (out.length >= k) break;
  }
  return out;
}

/** A score for display: 4 significant decimals. */
export function fmtScore(x: number): string {
  if (!Number.isFinite(x)) return '—';
  const a = Math.abs(x);
  const s = a !== 0 && (a < 0.0001 || a >= 1e6) ? x.toExponential(3) : x.toFixed(4);
  return s.replace(/^-/, '−');
}

/**
 * Bar lengths (0–1) for a list of scores so that the best hit is longest whatever the
 * metric: scores are scaled between the worst and best shown, with a small floor.
 */
export function scoreBars(scores: number[], metric: Metric): number[] {
  const finite = scores.filter(Number.isFinite);
  if (!finite.length) return scores.map(() => 0);
  const lo = Math.min(...finite);
  const hi = Math.max(...finite);
  const lower = metricInfo(metric).better === 'lower';
  return scores.map((s) => {
    if (!Number.isFinite(s)) return 0;
    if (hi === lo) return 1;
    const t = lower ? (hi - s) / (hi - lo) : (s - lo) / (hi - lo);
    return 0.15 + 0.85 * t;
  });
}

/** Run the search and keep the k best other entities. */
export async function similar(
  ds: string,
  self: string,
  q: SimilarQuery,
  signal?: AbortSignal,
): Promise<SimilarHit[]> {
  const rows = await api.select(ds, buildSimilarQuery(q), { signal });
  return similarHits(rows, self, q.k);
}

/** A plain-words explanation of a failed similarity search, by status. */
export function vectorErrorHint(e: unknown): { title: string; hint?: string } {
  if (!(e instanceof api.ApiError)) return { title: api.errorMessage(e) };
  switch (e.status) {
    case 400:
      return /dimension/i.test(e.message)
        ? {
            title: e.message,
            hint: 'Only vectors of the same dimension are compared. Pick another predicate or vector.',
          }
        : { title: e.message };
    case 507:
      return {
        title: 'The vector search budget is exhausted.',
        hint: e.budget
          ? `Packing these vectors needs about ${fmtBytes(e.budget.requested)} of the server's ${fmtBytes(e.budget.limit)} budget for packed vectors.`
          : e.message,
      };
    case 501:
      return { title: 'This server does not support this vector search.', hint: e.message };
    default:
      return { title: api.errorMessage(e) };
  }
}

// --- the Similar page: free-form searches ------------------------------------------

/** The largest `k` the page asks for, and the largest `ef` (docs/API.md). */
export const MAX_PAGE_K = 100;
export const MAX_EF = 4096;

export type VectorSearchQuery = {
  predicate: string;
  /** An entity whose single vector under the predicate is the query, or a vector's lexical form. */
  query: { entity: string } | { lexical: string };
  k: number;
  metric: Metric;
  /** HNSW candidates (`ef:N`); the index's `efSearch` when absent. */
  ef?: number;
  /** Search exactly even with an index (`exact:true`). */
  exact?: boolean;
  /** Ask for one row more than `k`, for leaving the query's own entity out. */
  skipSelf?: boolean;
};

/** `spk:vectorSearch` with the score and the matched vector, best first. */
export function buildVectorSearch(q: VectorSearchQuery): string {
  const k = Math.min(Math.max(1, Math.floor(q.k)) + (q.skipSelf ? 1 : 0), 10000);
  const query =
    'entity' in q.query
      ? sparqlIri(q.query.entity)
      : `"${q.query.lexical.trim().replace(/\s+/g, ' ')}"^^spk:vector`;
  const opts = [`"metric:${q.metric}"`];
  if (q.ef != null && Number.isFinite(q.ef)) opts.push(`"ef:${Math.floor(q.ef)}"`);
  if (q.exact) opts.push('"exact:true"');
  const order = metricInfo(q.metric).better === 'lower' ? 'ASC(?score)' : 'DESC(?score)';
  return `PREFIX spk: <${SPK}>
SELECT ?s ?score ?vector WHERE {
  (?s ?score ?vector) spk:vectorSearch (${sparqlIri(q.predicate)} ${query} ${k} ${opts.join(' ')}) .
}
ORDER BY ${order}`;
}

export type RankedHit = { term: Term; score: number; vector?: Term };

/** The rows as hits, best first, without `self` and at most `k` of them. */
export function rankedHits(
  rows: Record<string, Term | undefined>[],
  self: string | null,
  k: number,
): RankedHit[] {
  const out: RankedHit[] = [];
  for (const r of rows) {
    if (!r.s || (self != null && r.s.type === 'uri' && r.s.value === self)) continue;
    out.push({
      term: r.s,
      score: r.score?.type === 'literal' ? Number(r.score.value) : NaN,
      ...(r.vector ? { vector: r.vector } : {}),
    });
    if (out.length >= k) break;
  }
  return out;
}

export type VectorSearchResult = {
  hits: RankedHit[];
  /** How the server ran the search, when its plan says. */
  mode: SearchMode | null;
  /** The server's time for the query. */
  ms?: number;
};

/** Run a search and keep the `k` best rows other than `self`. */
export async function vectorSearch(
  ds: string,
  q: VectorSearchQuery,
  self: string | null,
  signal?: AbortSignal,
): Promise<VectorSearchResult> {
  const r = await api.query(ds, buildVectorSearch(q), { signal });
  const vars = r.vars ?? [];
  const rows = (r.rows ?? []).map((row) =>
    Object.fromEntries(vars.map((v, i) => [v, row[i] ?? undefined])),
  );
  return {
    hits: rankedHits(rows, self, q.k),
    mode: searchMode(r.meta?.plan),
    ms: r.meta?.timing?.totalMs,
  };
}

export type VectorInput =
  | { ok: true; values: number[]; lexical: string }
  | { ok: false; message: string; offset: number };

const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
const LITERAL = /^\s*"([^"]*)"\s*\^\^\s*(?:spk:vector|<urn:x-sparkles:vector>)\s*$/;

/**
 * A pasted query vector, either a JSON array of numbers (the lexical form of
 * `spk:vector`) or a whole literal `"[…]"^^spk:vector`. It checks the grammar of the
 * datatype and reports the first error with its offset in the text.
 */
export function parseVectorInput(text: string): VectorInput {
  const lit = LITERAL.exec(text);
  const body = lit ? lit[1] : text;
  const base = lit ? text.indexOf('"') + 1 : 0;
  const fail = (message: string, at: number): VectorInput => ({
    ok: false,
    message,
    offset: base + at,
  });
  let i = 0;
  const ws = () => {
    while (i < body.length && ' \t\n\r'.includes(body[i])) i++;
  };
  ws();
  if (i >= body.length) return fail('Paste a vector such as [0.1, -0.2, 0.3].', i);
  if (body[i] !== '[') return fail('A vector starts with "[".', i);
  i++;
  const values: number[] = [];
  for (;;) {
    ws();
    if (body[i] === ']' && values.length === 0) return fail('The vector is empty.', i);
    NUMBER.lastIndex = i;
    const m = NUMBER.exec(body);
    if (!m) return fail('Expected a number.', i);
    const x = Math.fround(Number(m[0]));
    if (!Number.isFinite(x)) return fail('The number is too large for a 32-bit float.', i);
    values.push(x);
    if (values.length > 16384) return fail('A vector has at most 16384 numbers.', i);
    i += m[0].length;
    ws();
    if (body[i] === ',') {
      i++;
      continue;
    }
    if (body[i] === ']') {
      i++;
      break;
    }
    return fail(
      i >= body.length ? 'The vector is not closed with "]".' : 'Expected "," or "]".',
      i,
    );
  }
  ws();
  if (i < body.length) return fail('Unexpected text after "]".', i);
  return { ok: true, values, lexical: body.trim() };
}

/** How an executed vector search ran, from the counters of its plan. */
export type SearchMode = {
  method: string;
  exactBecause?: string;
  ef?: number;
  rows?: number;
  scored?: number;
};

export function searchMode(plan: api.PlanNode | undefined): SearchMode | null {
  if (!plan) return null;
  const c = plan.counters;
  if (plan.operator === 'VectorSearch' && c?.method != null) {
    const num = (v: unknown) => (typeof v === 'number' ? v : undefined);
    const mode: SearchMode = { method: String(c.method) };
    if (c.exactBecause != null) mode.exactBecause = String(c.exactBecause);
    if (num(c.ef) != null) mode.ef = num(c.ef);
    if (num(c.rows) != null) mode.rows = num(c.rows);
    if (num(c.scored) != null) mode.scored = num(c.scored);
    return mode;
  }
  for (const ch of plan.children ?? []) {
    const m = searchMode(ch);
    if (m) return m;
  }
  return null;
}

/** `hnsw ef=128`, `exact (exact:true is set)` and so on. */
export function searchModeText(m: SearchMode): string {
  if (m.method === 'hnsw') return m.ef ? `hnsw ef=${m.ef}` : 'hnsw';
  return m.exactBecause ? `${m.method} (${m.exactBecause})` : m.method;
}

// --- vector index cards -------------------------------------------------------------

/** Index names: 1–64 letters, digits, `_`, `-` or `.`, not starting with `.`. */
export const INDEX_NAME = /^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,63}$/;

export const HNSW_DEFAULTS = { m: 16, efConstruction: 128, efSearch: 128 };
export const EXACT_THRESHOLD_DEFAULT = 10000;

/** The fields of the create and edit dialog, as typed. */
export type IndexForm = {
  name: string;
  predicate: string;
  dimension: string;
  metric: Metric;
  model: string;
  hnsw: boolean;
  m: string;
  efConstruction: string;
  efSearch: string;
  exactThreshold: string;
  /** The `embedding` object as JSON text; empty for an index of stored vectors. */
  embedding: string;
};

/** The form for a new index, or for editing `s`. */
export function indexForm(
  s?: api.VectorIndexStatus,
  prefixes: Record<string, string> = {},
): IndexForm {
  const h = s?.hnsw ?? HNSW_DEFAULTS;
  return {
    name: s?.name ?? '',
    predicate: s ? displayIri(s.predicate, prefixes) : '',
    dimension: s ? String(s.dimension) : '',
    metric: s?.metric ?? 'cosine',
    model: s?.model ?? '',
    hnsw: s ? s.hnsw != null : true,
    m: String(h.m),
    efConstruction: String(h.efConstruction),
    efSearch: String(h.efSearch),
    exactThreshold: String(s?.exactThreshold ?? EXACT_THRESHOLD_DEFAULT),
    embedding: s?.embedding ? JSON.stringify(s.embedding.config, null, 2) : '',
  };
}

/** A starting point for the `embedding` object of a new index (Ollama on this machine). */
export const EMBEDDING_EXAMPLE = JSON.stringify(
  {
    url: 'http://127.0.0.1:11434/v1/embeddings',
    model: 'nomic-embed-text',
    predicates: ['http://www.w3.org/2000/01/rdf-schema#label'],
  },
  null,
  2,
);

/** The `embedding` object typed in the dialog, or why it is not one. */
export function parseEmbedding(
  text: string,
): { config: api.EmbeddingConfig | null; error: null } | { config: null; error: string } {
  if (!text.trim()) return { config: null, error: null };
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch (e) {
    return { config: null, error: `Not JSON: ${(e as Error).message}` };
  }
  if (!v || typeof v !== 'object' || Array.isArray(v))
    return { config: null, error: 'A JSON object with url, model and predicates or query' };
  const o = v as Record<string, unknown>;
  if (typeof o.url !== 'string' || !/^https?:\/\/\S+$/.test(o.url))
    return { config: null, error: 'url: an http(s) URL' };
  if (typeof o.model !== 'string' || !o.model) return { config: null, error: 'model: required' };
  if (o.predicates == null && o.query == null)
    return { config: null, error: 'predicates or query: name the text to embed' };
  if (o.apiKey != null && (typeof o.apiKey !== 'object' || !('secret' in o.apiKey)))
    return {
      config: null,
      error: 'apiKey: {"secret": NAME}, a secret the server defines with --embedding-secret',
    };
  return { config: o as api.EmbeddingConfig, error: null };
}

/** The badge class of an embedding state. */
export function embeddingStateClass(s: api.EmbeddingState): string {
  return s === 'idle' ? 'ok' : s === 'backoff' || s === 'disabled' ? 'danger' : 'warn';
}

/** "3 waiting · applied to commit 41 of 42", or "caught up". */
export function embeddingProgress(e: api.EmbeddingStatus): string {
  if (e.appliedSeq >= e.headSeq && !e.backlog && !e.scan) return 'caught up with every commit';
  const parts: string[] = [];
  if (e.scan) parts.push(`scanning ${e.scan.done} of ${e.scan.total} subjects`);
  if (e.backlog) parts.push(`${e.backlog} waiting`);
  parts.push(`embedded up to commit ${e.appliedSeq} of ${e.headSeq}`);
  return parts.join(' · ');
}

/** The IRI of a predicate typed as an IRI, `<IRI>` or a prefixed name, or null. */
export function resolvePredicate(text: string, prefixes: Record<string, string>): string | null {
  const t = text.trim();
  if (!t || /\s/.test(t)) return null;
  const angled = /^<([^<>\s]+)>$/.exec(t);
  if (angled) return angled[1];
  const pn = /^([A-Za-z][\w.-]*)?:([^\s<>]*)$/.exec(t);
  if (pn && prefixes[pn[1] ?? ''] != null) return prefixes[pn[1] ?? ''] + pn[2];
  return /^[a-z][a-z0-9+.-]*:[^\s<>"]+$/i.test(t) ? t : null;
}

function int(text: string, lo: number, hi: number): number | null {
  if (!/^\s*\d+\s*$/.test(text)) return null;
  const n = Number(text);
  return n >= lo && n <= hi ? n : null;
}

export type IndexFormErrors = Partial<Record<keyof IndexForm, string>>;

/** The configuration a form describes, or the errors of its fields. */
export function indexConfig(
  f: IndexForm,
  prefixes: Record<string, string>,
): { config: api.VectorIndexConfig; errors: null } | { config: null; errors: IndexFormErrors } {
  const errors: IndexFormErrors = {};
  if (!INDEX_NAME.test(f.name))
    errors.name = '1 to 64 letters, digits, "_", "-" or ".", not starting with "."';
  const predicate = resolvePredicate(f.predicate, prefixes);
  if (!predicate) errors.predicate = 'An IRI, or a prefixed name with a known prefix';
  const dimension = int(f.dimension, 1, 16384);
  if (dimension == null) errors.dimension = '1 to 16384';
  if (f.model.length > 256) errors.model = 'At most 256 characters';
  const m = int(f.m, 2, 128);
  const efConstruction = int(f.efConstruction, 1, MAX_EF);
  const efSearch = int(f.efSearch, 1, MAX_EF);
  if (f.hnsw) {
    if (m == null) errors.m = '2 to 128';
    if (efConstruction == null) errors.efConstruction = `1 to ${MAX_EF}`;
    if (efSearch == null) errors.efSearch = `1 to ${MAX_EF}`;
  }
  const exactThreshold = int(f.exactThreshold, 0, Number.MAX_SAFE_INTEGER);
  if (exactThreshold == null) errors.exactThreshold = 'A whole number of rows';
  const embedding = parseEmbedding(f.embedding ?? '');
  if (embedding.error) errors.embedding = embedding.error;
  if (Object.keys(errors).length) return { config: null, errors };
  return {
    config: {
      predicate: predicate!,
      dimension: dimension!,
      metric: f.metric,
      ...(f.model.trim() ? { model: f.model.trim() } : {}),
      hnsw: f.hnsw ? { m: m!, efConstruction: efConstruction!, efSearch: efSearch! } : false,
      exactThreshold: exactThreshold!,
      ...(embedding.config ? { embedding: embedding.config } : {}),
    },
    errors: null,
  };
}

/**
 * Whether replacing `old` with `config` builds the index again. Only `efSearch`, the
 * exact threshold and the model can change without a build.
 */
export function needsBuild(old: api.VectorIndexStatus, config: api.VectorIndexConfig): boolean {
  const h = config.hnsw || null;
  if (old.predicate !== config.predicate || old.dimension !== config.dimension) return true;
  if (old.metric !== (config.metric ?? 'cosine')) return true;
  if ((old.hnsw == null) !== (h == null)) return true;
  return !!(old.hnsw && h && (old.hnsw.m !== h.m || old.hnsw.efConstruction !== h.efConstruction));
}

/** The changes that searches add exactly, as a share of the indexed rows. */
export function overlayShare(s: api.VectorIndexStatus): number {
  return (s.overlay.inserts + s.overlay.deletes) / Math.max(1, s.rows);
}

/** A recall measurement kept in the browser, with the time it was taken. */
export type RememberedRecall = api.VectorRecall & { at: string };

export const recallKey = (ds: string, index: string) => `sparkles.vectorRecall.${ds}.${index}`;

/** `0.987` as `98.7 %`. */
export function fmtRecall(r: number): string {
  if (!Number.isFinite(r)) return '—';
  return `${(Math.round(r * 1000) / 10).toFixed(1)} %`;
}
