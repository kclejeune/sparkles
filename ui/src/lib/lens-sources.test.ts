import { describe, expect, it } from 'vitest';
import type { QueryOptions, Term } from './api';
import {
  BATCH,
  checkable,
  chunk,
  classQuery,
  DEFAULT_GRAPH,
  degreeQuery,
  drawnDegrees,
  graphQuery,
  IN_FLIGHT,
  inferredPerNode,
  numberOf,
  pickCommon,
  pickGraph,
  pickNumber,
  pool,
  propertyQuery,
  readDegrees,
  readInferred,
  readValues,
  SEVERAL_GRAPHS,
  type LensEdge,
  type LensNode,
  type SelectFn,
  type ValueCache,
} from './lens-sources';

const iri = (v: string): Term => ({ type: 'uri', value: v });
const lit = (v: string): Term => ({ type: 'literal', value: v });
const EX = 'http://example.org/';
const node = (n: string): LensNode => ({ id: `<${EX}${n}>`, term: iri(EX + n) });

type Call = { sparql: string; opts: QueryOptions };

/** A query function that records its calls and answers from `answer`. */
function recorder(
  answer: (sparql: string, opts: QueryOptions) => Record<string, Term>[] = () => [],
) {
  const calls: Call[] = [];
  let inFlight = 0;
  let most = 0;
  const select: SelectFn = async (_ds, sparql, opts) => {
    calls.push({ sparql, opts });
    inFlight++;
    most = Math.max(most, inFlight);
    await new Promise((r) => setTimeout(r, 1));
    inFlight--;
    return answer(sparql, opts);
  };
  return { calls, select, most: () => most };
}

describe('batches', () => {
  it('cuts lists into batches of 200 and keeps at most four in flight', async () => {
    expect(chunk(Array.from({ length: 450 }, (_, i) => i)).map((c) => c.length)).toEqual([
      200, 200, 50,
    ]);
    let now = 0;
    let most = 0;
    const out = await pool(
      Array.from({ length: 10 }, (_, i) => i),
      async (i) => {
        now++;
        most = Math.max(most, now);
        await new Promise((r) => setTimeout(r, 2));
        now--;
        return i * 2;
      },
    );
    expect(out).toEqual([0, 2, 4, 6, 8, 10, 12, 14, 16, 18]);
    expect(most).toBe(IN_FLIGHT);
  });

  it('reads 1,000 nodes in five queries of at most 200, four at once', async () => {
    const nodes = Array.from({ length: 1000 }, (_, i) => node(`n${i}`));
    const r = recorder();
    await readValues(
      { ds: 'ds', select: r.select },
      nodes,
      (b) => propertyQuery(b, `${EX}age`),
      new Map(),
    );
    expect(r.calls.length).toBe(5);
    for (const c of r.calls)
      expect(c.sparql.match(/<http:\/\/example.org\/n\d+>/g)!.length).toBe(BATCH);
    expect(r.most()).toBe(IN_FLIGHT);
  });
});

describe('query text', () => {
  const q = [`${EX}a`, `${EX}b`];
  it('names the nodes in one VALUES block', () => {
    expect(degreeQuery(q)).toBe(
      `SELECT ?n (COUNT(*) AS ?c) WHERE { VALUES ?n { <${EX}a> <${EX}b> } { ?n ?p ?o } UNION { ?s ?p ?n } } GROUP BY ?n`,
    );
    expect(propertyQuery(q, `${EX}age`)).toContain(`?n <${EX}age> ?v`);
    expect(classQuery(q)).toContain('<http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ?v');
    expect(graphQuery(q)).toContain('FILTER(?v != <urn:x-sparkles:inferred>)');
  });
});

describe('incremental reads', () => {
  it('reads only the nodes the cache lacks, and remembers nodes without values', async () => {
    const r = recorder((sparql) =>
      sparql.includes(`<${EX}a>`) ? [{ n: iri(`${EX}a`), v: lit('41') }] : [],
    );
    const cache: ValueCache = new Map();
    const ctx = { ds: 'ds', at: 'commit:3', select: r.select };
    await readValues(ctx, [node('a'), node('b')], (b) => propertyQuery(b, `${EX}age`), cache);
    expect(r.calls.length).toBe(1);
    expect(r.calls[0].opts.at).toBe('commit:3');
    expect(cache.get(`<${EX}b>`)).toEqual([]);
    await readValues(
      ctx,
      [node('a'), node('b'), node('c')],
      (b) => propertyQuery(b, `${EX}age`),
      cache,
    );
    expect(r.calls.length).toBe(2);
    expect(r.calls[1].sparql).toContain(`<${EX}c>`);
    expect(r.calls[1].sparql).not.toContain(`<${EX}a>`);
    // a third read of the same nodes reads nothing
    await readValues(ctx, [node('a'), node('c')], (b) => propertyQuery(b, `${EX}age`), cache);
    expect(r.calls.length).toBe(2);
    expect(pickNumber([node('a'), node('b')], cache)).toEqual(
      new Map([
        [`<${EX}a>`, 41],
        [`<${EX}b>`, null],
      ]),
    );
  });

  it('skips literals and blank nodes, which a VALUES block cannot name', async () => {
    const r = recorder();
    await readValues(
      { ds: 'ds', select: r.select },
      [
        { id: 'lit:1', term: lit('x') },
        { id: '_:b', term: { type: 'bnode', value: 'b' } },
      ],
      classQuery,
      new Map(),
    );
    expect(r.calls.length).toBe(0);
  });

  it('reads dataset degrees, zero for a node without rows', async () => {
    const r = recorder(() => [{ n: iri(`${EX}a`), c: lit('12') }]);
    const d = await readDegrees({ ds: 'ds', select: r.select }, [node('a'), node('b')], new Map());
    expect(d.get(`<${EX}a>`)).toBe(12);
    expect(d.get(`<${EX}b>`)).toBe(0);
  });
});

describe('picking values', () => {
  it('counts drawn degrees in the browser', () => {
    const nodes = [node('a'), node('b'), node('c')];
    const edges: LensEdge[] = [
      { id: '1', source: nodes[0].id, target: nodes[1].id },
      { id: '2', source: nodes[0].id, target: nodes[2].id },
      { id: '3', source: nodes[0].id, target: nodes[0].id },
    ];
    expect([...drawnDegrees(nodes, edges).values()]).toEqual([3, 1, 1]);
  });

  it('gives a node with several categories the one most drawn nodes share', () => {
    const nodes = [node('a'), node('b'), node('c')];
    const lists = new Map([
      [nodes[0].id, ['Manager', 'Person']],
      [nodes[1].id, ['Person']],
      [nodes[2].id, []],
    ]);
    expect([...pickCommon(nodes, lists).values()]).toEqual(['Person', 'Person', null]);
  });

  it('names one graph, several graphs or the default graph', () => {
    const nodes = [node('a'), node('b'), node('c'), { id: 'lit', term: lit('x') }];
    const cache: ValueCache = new Map([
      [nodes[0].id, [iri(`${EX}g1`)]],
      [nodes[1].id, [iri(`${EX}g1`), iri(`${EX}g2`)]],
      [nodes[2].id, []],
    ]);
    expect([...pickGraph(nodes, cache).values()]).toEqual([
      `${EX}g1`,
      SEVERAL_GRAPHS,
      DEFAULT_GRAPH,
      null,
    ]);
  });

  it('reads numbers from literals only', () => {
    expect(numberOf(lit('3.5'))).toBe(3.5);
    expect(numberOf(lit('-2e3'))).toBe(-2000);
    expect(numberOf(lit('three'))).toBeNull();
    expect(numberOf(iri(`${EX}3`))).toBeNull();
  });
});

describe('the inferred-edge comparison', () => {
  const alice = iri(`${EX}alice`);
  const type = iri('http://www.w3.org/1999/02/22-rdf-syntax-ns#type');
  const edges: LensEdge[] = [
    { id: 'e1', source: 'a', target: 'm', triple: [alice, type, iri(`${EX}Manager`)] },
    { id: 'e2', source: 'a', target: 't', triple: [alice, iri(`${EX}manages`), iri(`${EX}team7`)] },
    {
      id: 'e3',
      source: 'a',
      target: 'b',
      triple: [alice, iri(`${EX}knows`), { type: 'bnode', value: 'x' }],
    },
    { id: 'e4', source: 'a', target: 'c', triple: [alice, iri('urn:var:o'), iri(`${EX}c`)] },
  ];
  /** The data without reasoning holds e2 only, and with reasoning e1 and e2. */
  const answer = (sparql: string, opts: QueryOptions) => {
    const rows: Record<string, Term>[] = [];
    if (sparql.includes('manages'))
      rows.push({ s: alice, p: iri(`${EX}manages`), o: iri(`${EX}team7`) });
    if (opts.reasoning !== false && sparql.includes('Manager'))
      rows.push({ s: alice, p: type, o: iri(`${EX}Manager`) });
    return rows;
  };

  it('marks the edges absent with reasoning=false, and leaves those it cannot name unchecked', async () => {
    const r = recorder(answer);
    const cache = new Map<string, boolean>();
    const { inferred, unchecked } = await readInferred(
      { ds: 'ds', select: r.select },
      edges,
      cache,
    );
    expect([...inferred]).toEqual(['e1']);
    expect([...unchecked].sort()).toEqual(['e3', 'e4']);
    expect(r.calls.length).toBe(1);
    expect(r.calls[0].opts.reasoning).toBe(false);
    expect(r.calls[0].sparql).toContain('VALUES (?s ?p ?o)');
    // the answers are kept: a second check reads nothing
    await readInferred({ ds: 'ds', select: r.select }, edges, cache);
    expect(r.calls.length).toBe(1);
    expect([...inferredPerNode(edges, inferred)]).toEqual([
      ['a', 1],
      ['m', 1],
    ]);
  });

  it('with verify, an edge absent both ways is not inferred', async () => {
    const made: LensEdge = {
      id: 'e5',
      source: 'a',
      target: 'z',
      triple: [alice, iri(`${EX}invented`), iri(`${EX}z`)],
    };
    const r = recorder(answer);
    const { inferred } = await readInferred(
      { ds: 'ds', select: r.select },
      [...edges, made],
      new Map(),
      true,
    );
    expect([...inferred]).toEqual(['e1']);
    expect(r.calls.map((c) => c.opts.reasoning).sort()).toEqual([false, undefined]);
  });

  it('names an edge only when every term can stand in VALUES', () => {
    expect(checkable(edges[0].triple)).toBe(true);
    expect(checkable(edges[2].triple)).toBe(false);
    expect(checkable(edges[3].triple)).toBe(false);
    expect(checkable(undefined)).toBe(false);
  });
});
