import { describe, expect, it } from 'vitest';
import { ApiError, type Term } from './api';
import { displayTerm } from './rdf';
import {
  abbreviateVector,
  buildSimilarQuery,
  fmtScore,
  isVectorLiteral,
  metricInfo,
  parseVector,
  scoreBars,
  similarHits,
  VECTOR_DATATYPE,
  vectorErrorHint,
  vectorPredicates,
} from './vectors';

const vec = (value: string): Term => ({ type: 'literal', value, datatype: VECTOR_DATATYPE });

describe('vector literals', () => {
  it('detects the vector datatype', () => {
    expect(isVectorLiteral(vec('[1, 2]'))).toBe(true);
    expect(isVectorLiteral({ type: 'literal', value: '[1, 2]' })).toBe(false);
    expect(isVectorLiteral({ type: 'uri', value: VECTOR_DATATYPE })).toBe(false);
    expect(isVectorLiteral(null)).toBe(false);
  });

  it('parses JSON arrays of finite numbers only', () => {
    expect(parseVector('[0.1, -0.2, 3]')).toEqual([0.1, -0.2, 3]);
    expect(parseVector('[]')).toBeNull();
    expect(parseVector('[1, "x"]')).toBeNull();
    expect(parseVector('[1, 2')).toBeNull();
    expect(parseVector('{"a": 1}')).toBeNull();
  });

  it('abbreviates to the dimension and the first components', () => {
    const long = JSON.stringify(Array.from({ length: 384 }, (_, i) => (i % 2 ? -0.03 : 0.12)));
    expect(abbreviateVector(long)).toBe('vector(384) [0.12, −0.03, 0.12, …]');
    expect(abbreviateVector('[1, 2]')).toBe('vector(2) [1, 2]');
    expect(abbreviateVector('[0.123456, 0.0000123, 123456]')).toBe(
      'vector(3) [0.123, 1.2e-5, 1.2e+5]',
    );
    expect(abbreviateVector('[1, 2, oops]')).toBe('vector(invalid) [1, 2, oops]');
  });

  it('is abbreviated wherever terms are displayed as text', () => {
    expect(displayTerm(vec('[1, 2, 3, 4]'), {})).toBe('vector(4) [1, 2, 3, …]');
    expect(displayTerm({ type: 'literal', value: '[1, 2, 3, 4]' }, {})).toBe('[1, 2, 3, 4]');
  });

  it('groups an entity’s vector properties by predicate', () => {
    const preds = vectorPredicates([
      { p: 'http://ex/emb', o: vec('[1, 2, 3]') },
      { p: 'http://ex/label', o: { type: 'literal', value: 'x' } },
      { p: 'http://ex/emb', o: vec('[1, 2]') },
      { p: 'http://ex/b', o: vec('[1]') },
    ]);
    expect(preds.map((p) => [p.iri, p.vectors.length, p.dimensions])).toEqual([
      ['http://ex/b', 1, [1]],
      ['http://ex/emb', 2, [3, 2]],
    ]);
  });
});

describe('similarity search', () => {
  it('queries by entity for k + 1 hits, best first', () => {
    const q = buildSimilarQuery({
      predicate: 'http://ex/emb',
      query: { entity: 'http://ex/doc 3' },
      k: 10,
      metric: 'cosine',
    });
    expect(q).toContain('PREFIX spk: <urn:x-sparkles:>');
    expect(q).toContain(
      '(?s ?score) spk:vectorSearch (<http://ex/emb> <http://ex/doc%203> 11 "metric:cosine") .',
    );
    expect(q).toContain('ORDER BY DESC(?score)');
  });

  it('orders euclidean distances ascending and accepts a vector literal', () => {
    const q = buildSimilarQuery({
      predicate: 'http://ex/emb',
      query: { vector: vec('[1, 0]') as Extract<Term, { type: 'literal' }> },
      k: 5,
      metric: 'euclidean',
    });
    expect(q).toContain('(<http://ex/emb> "[1, 0]"^^<urn:x-sparkles:vector> 6 "metric:euclidean")');
    expect(q).toContain('ORDER BY ASC(?score)');
    expect(metricInfo('euclidean').better).toBe('lower');
    expect(metricInfo('dot').better).toBe('higher');
  });

  it('drops the entity itself and keeps k hits', () => {
    const row = (s: string, score: number) => ({
      s: { type: 'uri', value: s } as Term,
      score: { type: 'literal', value: String(score) } as Term,
    });
    const rows = [row('http://ex/me', 1), row('http://ex/a', 0.9), row('http://ex/b', 0.8)];
    expect(similarHits(rows, 'http://ex/me', 1)).toEqual([
      { term: { type: 'uri', value: 'http://ex/a' }, score: 0.9 },
    ]);
    // not in the list (e.g. a tie pushed it out): the first k are kept
    expect(similarHits(rows.slice(1), 'http://ex/me', 5)).toHaveLength(2);
  });

  it('formats scores and draws the best hit longest for every metric', () => {
    expect(fmtScore(0.98765)).toBe('0.9877');
    expect(fmtScore(-1.5)).toBe('−1.5000');
    expect(fmtScore(NaN)).toBe('—');
    expect(scoreBars([0.9, 0.5, 0.1], 'cosine')).toEqual([1, 0.575, 0.15]);
    const d = scoreBars([0.1, 0.5, 0.9], 'euclidean');
    expect(d[0]).toBe(1);
    expect(d[2]).toBeCloseTo(0.15);
    expect(scoreBars([2, 2], 'dot')).toEqual([1, 1]);
  });

  it('explains dimension, budget and unsupported errors', () => {
    expect(
      vectorErrorHint(
        new ApiError(400, 'dimension mismatch: the predicate has vectors of dimension 3'),
      ).hint,
    ).toContain('same dimension');
    const budget = vectorErrorHint(
      new ApiError(507, 'budget', { budget: { kind: 'memory', limit: 1024, requested: 4096 } }),
    );
    expect(budget.title).toContain('budget');
    expect(budget.hint).toContain('4.0 KiB');
    expect(vectorErrorHint(new ApiError(501, 'not supported')).title).toContain('does not support');
  });
});
