import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ShaclResult } from './api';
import {
  badges,
  componentKind,
  countsText,
  focusable,
  fromShacl,
  fromShex,
  groupResults,
  literalNodeId,
  nodesOf,
  probeGuard,
  resolution,
  resultsByNode,
  validateNodes,
  type VResult,
  type WritePreview,
} from './validation-overlay';

const EX = 'http://example.org/';
const SH = 'http://www.w3.org/ns/shacl#';
const uri = (v: string) => ({ type: 'uri' as const, value: v });
const lit = (v: string) => ({ type: 'literal' as const, value: v });

const r = (over: Partial<ShaclResult> = {}): VResult =>
  fromShacl({
    focusNode: uri(EX + 'p17'),
    resultPath: uri(EX + 'email'),
    value: lit('a@x'),
    sourceShape: uri(EX + 'PersonShape'),
    sourceConstraintComponent: uri(SH + 'PatternConstraintComponent'),
    severity: uri(SH + 'Violation'),
    messages: ['not an email address'],
    ...over,
  });

afterEach(() => vi.unstubAllGlobals());

describe('results', () => {
  it('normalizes SHACL and ShEx results', () => {
    expect(componentKind(SH + 'MinCountConstraintComponent')).toBe('minCount');
    const x = r();
    expect(x).toMatchObject({ kind: 'pattern', path: EX + 'email', severity: 'violation' });
    expect(r({ severity: uri(SH + 'Warning') }).severity).toBe('warning');
    const s = fromShex({
      node: uri(EX + 'p1'),
      shape: { type: 'start' },
      status: 'nonconformant',
      reason: 'why',
      appinfo: {
        failures: [
          { kind: 'cardinality', predicate: EX + 'name', inverse: false, min: 1, max: 1, count: 0 },
          { kind: 'datatype', value: lit('x'), constraint: 'xsd:integer' },
        ],
      },
    });
    expect(s.map((v) => v.kind)).toEqual(['cardinality', 'datatype']);
    expect(s[0]).toMatchObject({ shape: 'START', path: EX + 'name', min: 1, count: 0 });
    expect(fromShex({ node: uri(EX + 'p1'), shape: uri(EX + 'S'), status: 'conformant' })).toEqual(
      [],
    );
  });

  it('maps results to the drawn focus node and the literal drawn as a value', () => {
    const lid = literalNodeId(uri(EX + 'p17'), EX + 'email', lit('a@x'));
    const drawn = new Set([`<${EX}p17>`, lid]);
    expect(nodesOf(r(), drawn)).toEqual([`<${EX}p17>`, lid]);
    // a path of more than one predicate: any literal of the focus with the value
    expect(nodesOf(r({ resultPath: { type: 'path', value: 'x/y' } }), drawn)).toEqual([
      `<${EX}p17>`,
      lid,
    ]);
    // not drawn
    expect(nodesOf(r({ focusNode: uri(EX + 'other') }), drawn)).toEqual([]);
  });

  it('badges carry the count and the worst severity', () => {
    const drawn = new Set([`<${EX}p17>`, `<${EX}p18>`]);
    const rs = [
      r({ severity: uri(SH + 'Warning'), value: null }),
      r({ value: null }),
      r({ focusNode: uri(EX + 'p18'), severity: uri(SH + 'Info'), value: null }),
    ];
    const b = badges(resultsByNode(rs, drawn));
    expect(b.get(`<${EX}p17>`)).toEqual({
      count: 2,
      worst: 'violation',
      text: '✕2',
      words: '1 violation, 1 warning',
    });
    expect(b.get(`<${EX}p18>`)?.text).toBe('ⓘ1');
    expect(countsText([r(), r()])).toBe('2 violations');
    expect(groupResults(rs).length).toBe(1);
    expect(groupResults([...rs, r({ sourceShape: uri(EX + 'Other') })]).length).toBe(2);
  });

  it('validates IRIs and blank nodes only, once each', () => {
    expect(focusable([uri('a'), lit('x'), uri('a'), { type: 'bnode', value: 'b1' }])).toEqual([
      uri('a'),
      { type: 'bnode', value: 'b1' },
    ]);
  });
});

describe('resolution', () => {
  const preview = (results: Record<string, unknown>[], truncated = false): WritePreview => ({
    dryRun: true,
    wouldCommit: true,
    outcome: 'commit',
    head: 3,
    validation: { language: 'shacl', results, truncated },
  });
  it('reads the guard summary of a preview', () => {
    const x = r();
    const same = {
      focusNode: uri(EX + 'p17'),
      sourceShape: uri(EX + 'PersonShape'),
      sourceConstraintComponent: uri(SH + 'PatternConstraintComponent'),
      resultPath: uri(EX + 'email'),
    };
    expect(resolution(x, preview([same]))).toBe('remains');
    expect(resolution(x, preview([]))).toBe('resolved');
    expect(resolution(x, preview([], true))).toBe('unknown');
    expect(resolution(x, { ...preview([]), validation: undefined })).toBe('unknown');
  });
});

describe('requests', () => {
  it('sends batches of focus nodes, one at a time, to the stored shapes', async () => {
    const calls: { url: string; body: string }[] = [];
    let inFlight = 0;
    let most = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init: RequestInit) => {
        inFlight++;
        most = Math.max(most, inFlight);
        calls.push({ url, body: String(init.body) });
        await new Promise((res) => setTimeout(res, 1));
        inFlight--;
        return new Response(JSON.stringify({ conforms: true, results: [] }), {
          headers: { 'Content-Type': 'application/json' },
        });
      }),
    );
    const nodes = Array.from({ length: 1200 }, (_, i) => uri(`${EX}n${i}`));
    await validateNodes('ds', { kind: 'guard' }, 'shacl', nodes, { at: 'commit:3' });
    expect(calls.length).toBe(3);
    expect(most).toBe(1);
    expect(calls[0].url).toContain('/ds/shacl?at=commit%3A3&shapes=guard');
    expect(new URLSearchParams(calls[0].body).getAll('target').length).toBe(500);
    expect(new URLSearchParams(calls[2].body).getAll('target').length).toBe(200);
  });

  it('asks a ShEx guard with START for every node', async () => {
    let body = '';
    let url = '';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (u: string, init: RequestInit) => {
        url = u;
        body = String(init.body);
        return new Response(
          JSON.stringify({ conforms: false, counts: {}, results: [], warnings: [], millis: 0 }),
        );
      }),
    );
    await validateNodes('ds', { kind: 'guard' }, 'shex', [
      uri(EX + 'a'),
      { type: 'bnode', value: 'b1' },
    ]);
    expect(url).toContain('/ds/shex?schema=guard&results=nonconformant');
    expect(new URLSearchParams(body).get('map')).toBe(`<${EX}a>@START,_:b1@START`);
  });

  it('probes the guard', async () => {
    const answer = (status: number, error: string) =>
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => new Response(JSON.stringify({ error }), { status })),
      );
    answer(404, '/ds has no write-time validation');
    expect(await probeGuard('ds')).toEqual({ state: 'none' });
    answer(400, 'the write-time guard of /ds validates ShEx: use /ds/shex?schema=guard');
    expect(await probeGuard('ds')).toEqual({ state: 'shex' });
    answer(403, 'your access is limited to some graphs');
    expect(await probeGuard('ds')).toEqual({
      state: 'restricted',
      message: 'Validation needs access to every graph',
    });
  });
});
