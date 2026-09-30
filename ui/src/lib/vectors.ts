// Vector literals (`urn:x-sparkles:vector`) and similarity search (`spk:vectorSearch`),
// docs/API.md "Vector similarity".
import * as api from './api';
import type { Term } from './api';
import { fmtBytes } from './format';
import {
  abbreviateVector,
  isVectorLiteral,
  literalText,
  parseVector,
  sparqlIri,
  SPK,
  toSparql,
  VECTOR_DATATYPE,
} from './rdf';

// The literal helpers live in rdf.ts (term display uses them); re-exported here.
export { abbreviateVector, isVectorLiteral, literalText, parseVector, SPK, VECTOR_DATATYPE };

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
        ...new Set(vectors.map((v) => parseVector(v.value)?.length).filter((n) => n != null)),
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
