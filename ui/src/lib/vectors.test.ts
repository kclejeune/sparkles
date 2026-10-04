import { describe, expect, it } from 'vitest';
import { ApiError, type PlanNode, type Term, type VectorIndexStatus } from './api';
import { displayTerm } from './rdf';
import {
  abbreviateVector,
  buildSimilarQuery,
  buildVectorSearch,
  embeddingProgress,
  embeddingStateClass,
  fmtRecall,
  fmtScore,
  indexConfig,
  indexForm,
  isVectorLiteral,
  literalText,
  metricInfo,
  needsBuild,
  overlayShare,
  parseEmbedding,
  parseVector,
  parseVectorInput,
  rankedHits,
  resolvePredicate,
  scoreBars,
  searchMode,
  searchModeText,
  similarHits,
  VECTOR_B64_DATATYPE,
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

  it('reads the compact datatype', () => {
    const b64 = {
      type: 'literal' as const,
      value: 'zczMPc3MTD6amZk+',
      datatype: VECTOR_B64_DATATYPE,
    };
    expect(isVectorLiteral(b64)).toBe(true);
    const v = parseVector(b64.value, VECTOR_B64_DATATYPE)!;
    expect(v.map((x) => +x.toFixed(6))).toEqual([0.1, 0.2, 0.3]);
    expect(literalText(b64)).toBe('vector(3) [0.1, 0.2, 0.3]');
    for (const bad of ['', 'AAA', 'AAAA', 'zczM Pc3M', 'AAAAAAA=', 'AACAfw=='])
      expect(parseVector(bad, VECTOR_B64_DATATYPE), bad).toBeNull();
    // the JSON form is not read as base64, nor the other way round
    expect(parseVector('[1]', VECTOR_B64_DATATYPE)).toBeNull();
    expect(parseVector('AACAPw==')).toBeNull();
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

describe('the Similar page', () => {
  it('builds searches with the matched vector, ef and exact', () => {
    const q = buildVectorSearch({
      predicate: 'http://ex/emb',
      query: { entity: 'http://ex/a' },
      k: 10,
      metric: 'dot',
      ef: 64,
      exact: true,
      skipSelf: true,
    });
    expect(q).toContain('SELECT ?s ?score ?vector WHERE');
    expect(q).toContain(
      '(?s ?score ?vector) spk:vectorSearch (<http://ex/emb> <http://ex/a> 11 "metric:dot" "ef:64" "exact:true") .',
    );
    expect(q).toContain('ORDER BY DESC(?score)');
    const v = buildVectorSearch({
      predicate: 'http://ex/emb',
      query: { lexical: '[1,\n 2.5e-3]' },
      k: 5,
      metric: 'euclidean',
    });
    expect(v).toContain('(<http://ex/emb> "[1, 2.5e-3]"^^spk:vector 5 "metric:euclidean")');
    expect(v).toContain('ORDER BY ASC(?score)');
  });

  it('ranks the rows without the query entity', () => {
    const row = (s: string, score: number, v?: string) => ({
      s: { type: 'uri', value: s } as Term,
      score: { type: 'literal', value: String(score) } as Term,
      ...(v ? { vector: vec(v) } : {}),
    });
    const hits = rankedHits(
      [row('http://ex/me', 1), row('http://ex/a', 0.9, '[1]'), row('http://ex/b', 0.8)],
      'http://ex/me',
      1,
    );
    expect(hits).toEqual([
      { term: { type: 'uri', value: 'http://ex/a' }, score: 0.9, vector: vec('[1]') },
    ]);
    expect(rankedHits([row('http://ex/me', 1)], null, 5)).toHaveLength(1);
  });

  it('checks pasted vectors against the grammar, with the offset of the first error', () => {
    expect(parseVectorInput(' [0.5, -1, 2e3] ')).toEqual({
      ok: true,
      values: [0.5, -1, 2000],
      lexical: '[0.5, -1, 2e3]',
    });
    const lit = parseVectorInput('"[1, 2]"^^spk:vector');
    expect(lit.ok && lit.lexical).toBe('[1, 2]');
    expect(parseVectorInput('"[1]"^^<urn:x-sparkles:vector>').ok).toBe(true);
    expect(parseVectorInput('')).toMatchObject({ ok: false, offset: 0 });
    expect(parseVectorInput('[]')).toMatchObject({ ok: false, message: 'The vector is empty.' });
    expect(parseVectorInput('[1, .5]')).toMatchObject({ ok: false, offset: 4 });
    expect(parseVectorInput('[1, +1]')).toMatchObject({ ok: false, offset: 4 });
    expect(parseVectorInput('[1 2]')).toMatchObject({ ok: false, message: 'Expected "," or "]".' });
    expect(parseVectorInput('[1, 2')).toMatchObject({ ok: false, offset: 5 });
    expect(parseVectorInput('[1] x')).toMatchObject({ ok: false, offset: 4 });
    expect(parseVectorInput('[1e39]')).toMatchObject({ ok: false, offset: 1 });
    expect(parseVectorInput('"[1, x]"^^spk:vector')).toMatchObject({ ok: false, offset: 5 });
  });

  it('reads how the search ran from the plan', () => {
    const node = (operator: string, counters?: PlanNode['counters'], children: PlanNode[] = []) =>
      ({ operator, counters, children }) as PlanNode;
    const hnsw = searchMode(
      node('Project', undefined, [
        node('VectorSearch', { method: 'hnsw', ef: 128, rows: 1000, scored: 140 }),
      ]),
    );
    expect(hnsw).toEqual({ method: 'hnsw', ef: 128, rows: 1000, scored: 140 });
    expect(searchModeText(hnsw!)).toBe('hnsw ef=128');
    const exact = searchMode(node('VectorSearch', { method: 'exact', exactBecause: 'few rows' }));
    expect(searchModeText(exact!)).toBe('exact (few rows)');
    expect(searchMode(node('Scan'))).toBeNull();
  });
});

describe('vector index cards', () => {
  const status: VectorIndexStatus = {
    name: 'emb',
    predicate: 'http://ex/emb',
    dimension: 384,
    metric: 'cosine',
    model: 'mini',
    state: 'ready',
    generation: 'gen-0001',
    rows: 1000,
    overlay: { inserts: 90, deletes: 20 },
    skipped: { malformed: 0, wrongDimension: 0, zeroNorm: 0 },
    memory: { segmentBytes: 1, hnswBytes: 1, residency: 'heap' },
    hnsw: { m: 16, efConstruction: 128, efSearch: 128, nodes: 1000, layers: 3 },
    exactThreshold: 10000,
  };
  const prefixes = { ex: 'http://ex/' };

  it('resolves predicates typed as IRIs or prefixed names', () => {
    expect(resolvePredicate('ex:emb', prefixes)).toBe('http://ex/emb');
    expect(resolvePredicate('<http://x/y>', prefixes)).toBe('http://x/y');
    expect(resolvePredicate('http://x/y', prefixes)).toBe('http://x/y');
    // an unknown prefix reads as the scheme of an absolute IRI, as urn: does
    expect(resolvePredicate('urn:emb', prefixes)).toBe('urn:emb');
    expect(resolvePredicate('<bad iri>', prefixes)).toBeNull();
    expect(resolvePredicate('two words', prefixes)).toBeNull();
  });

  it('turns the form into a configuration, or names the bad fields', () => {
    const f = indexForm(status, prefixes);
    expect(f.predicate).toBe('ex:emb');
    expect(indexConfig(f, prefixes).config).toEqual({
      predicate: 'http://ex/emb',
      dimension: 384,
      metric: 'cosine',
      model: 'mini',
      hnsw: { m: 16, efConstruction: 128, efSearch: 128 },
      exactThreshold: 10000,
    });
    expect(indexConfig({ ...f, hnsw: false, m: 'x' }, prefixes).config?.hnsw).toBe(false);
    const bad = indexConfig(
      { ...f, name: '.x', dimension: '0', m: '1', efSearch: '5000', exactThreshold: '-1' },
      prefixes,
    );
    expect(bad.config).toBeNull();
    expect(Object.keys(bad.errors ?? {}).sort()).toEqual([
      'dimension',
      'efSearch',
      'exactThreshold',
      'm',
      'name',
    ]);
    expect(indexForm().hnsw).toBe(true);
  });

  it('knows which changes build the index again', () => {
    const cfg = indexConfig(indexForm(status, prefixes), prefixes).config!;
    expect(needsBuild(status, cfg)).toBe(false);
    expect(
      needsBuild(status, { ...cfg, hnsw: { m: 16, efConstruction: 128, efSearch: 512 } }),
    ).toBe(false);
    expect(needsBuild(status, { ...cfg, exactThreshold: 5, model: 'other' })).toBe(false);
    expect(
      needsBuild(status, { ...cfg, hnsw: { m: 32, efConstruction: 128, efSearch: 128 } }),
    ).toBe(true);
    expect(needsBuild(status, { ...cfg, metric: 'dot' })).toBe(true);
    expect(needsBuild(status, { ...cfg, hnsw: false })).toBe(true);
    expect(needsBuild(status, { ...cfg, dimension: 768 })).toBe(true);
  });

  it('formats recall and the overlay share', () => {
    expect(fmtRecall(0.9876)).toBe('98.8 %');
    expect(fmtRecall(1)).toBe('100.0 %');
    expect(fmtRecall(NaN)).toBe('—');
    expect(overlayShare(status)).toBeCloseTo(0.11);
  });
});

describe('embedding settings', () => {
  const status: VectorIndexStatus = {
    name: 'docs',
    predicate: 'http://ex/emb',
    dimension: 8,
    metric: 'cosine',
    state: 'ready',
    generation: 'gen-0001',
    rows: 0,
    overlay: { inserts: 0, deletes: 0 },
    skipped: { malformed: 0, wrongDimension: 0, zeroNorm: 0 },
    memory: { segmentBytes: 0, hnswBytes: 0, residency: 'heap' },
    hnsw: null,
    exactThreshold: 10000,
    embedding: {
      state: 'embedding',
      model: 'nomic-embed-text',
      endpoint: 'http://127.0.0.1:11434/v1/embeddings',
      backlog: 3,
      appliedSeq: 40,
      headSeq: 42,
      embedded: 10,
      requests: 2,
      failed: 0,
      config: {
        url: 'http://127.0.0.1:11434/v1/embeddings',
        model: 'nomic-embed-text',
        predicates: ['http://www.w3.org/2000/01/rdf-schema#label'],
      },
    },
  };

  it('keeps the embedding object when an index is edited', () => {
    const f = indexForm(status, {});
    expect(JSON.parse(f.embedding).model).toBe('nomic-embed-text');
    const c = indexConfig(f, {});
    expect(c.config?.embedding?.predicates).toEqual(['http://www.w3.org/2000/01/rdf-schema#label']);
    expect(indexConfig({ ...f, embedding: '' }, {}).config?.embedding).toBeUndefined();
  });

  it('checks the JSON typed in the dialog', () => {
    expect(parseEmbedding('').config).toBeNull();
    expect(parseEmbedding('{').error).toMatch(/Not JSON/);
    expect(parseEmbedding('{"url":"ftp://x","model":"m","predicates":[]}').error).toMatch(/url/);
    expect(parseEmbedding('{"url":"http://x/","model":"m"}').error).toMatch(/predicates or query/);
    expect(
      parseEmbedding(
        '{"url":"http://x/","model":"m","query":"SELECT ?s ?text {}","apiKey":{"env":"K"}}',
      ).error,
    ).toMatch(/secret/);
    expect(
      parseEmbedding(
        '{"url":"http://x/","model":"m","predicates":["http://p"],"apiKey":{"secret":"k"}}',
      ).config?.apiKey,
    ).toEqual({ secret: 'k' });
  });

  it('describes the worker', () => {
    const e = status.embedding!;
    expect(embeddingProgress(e)).toBe('3 waiting · embedded up to commit 40 of 42');
    expect(embeddingProgress({ ...e, backlog: 0, appliedSeq: 42 })).toBe(
      'caught up with every commit',
    );
    expect(embeddingProgress({ ...e, scan: { done: 5, total: 9 } })).toMatch(/^scanning 5 of 9/);
    expect(embeddingStateClass('idle')).toBe('ok');
    expect(embeddingStateClass('backoff')).toBe('danger');
    expect(embeddingStateClass('paused')).toBe('warn');
  });
});
