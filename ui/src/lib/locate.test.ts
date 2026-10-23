import { describe, expect, it, vi } from 'vitest';
import type { Term } from './api';
import { GEOJSON_LITERAL, GML_LITERAL, LOCATE_BATCH, WKT_LITERAL } from './geo';
import { LOCATE_IN_FLIGHT, locateEntities } from './locate';

const iri = (value: string): Term => ({ type: 'uri', value });
const wkt = (value: string): Term => ({ type: 'literal', value, datatype: WKT_LITERAL });
const row = (s: string, lit: Term) => ({ s: iri(s), lit });
const never = async () => {
  throw new Error('convert should not run');
};

describe('locating entities', () => {
  it('reads the position of every entity with a geometry', async () => {
    const run = vi.fn(async () => [
      row('http://x/a', wkt('POINT(4.8 45.7)')),
      row('http://x/b', wkt('POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))')),
    ]);
    const out = await locateEntities(['http://x/a', 'http://x/b', 'http://x/c'], {
      run,
      convert: never,
    });
    expect(out.found.get('http://x/a')).toEqual([4.8, 45.7]);
    const b = out.found.get('http://x/b')!;
    expect(b[0]).toBeCloseTo(1, 9);
    expect(b[1]).toBeCloseTo(1, 9);
    // an entity without a geometry is simply not in the answer
    expect(out.found.has('http://x/c')).toBe(false);
    expect(out.failed).toBe(0);
  });

  it('asks about 200 entities at a time, four requests in flight', async () => {
    const iris = Array.from({ length: LOCATE_BATCH * 9 + 5 }, (_, i) => `http://x/e${i}`);
    let active = 0;
    let peak = 0;
    const sizes: number[] = [];
    const run = vi.fn(async (sparql: string) => {
      active++;
      peak = Math.max(peak, active);
      sizes.push((sparql.match(/<http:\/\/x\/e\d+>/g) ?? []).length);
      await new Promise((r) => setTimeout(r, 5));
      active--;
      return [];
    });
    await locateEntities(iris, { run, convert: never });
    expect(run).toHaveBeenCalledTimes(10);
    expect(peak).toBe(LOCATE_IN_FLIGHT);
    expect(Math.max(...sizes)).toBe(LOCATE_BATCH);
    expect(sizes.reduce((a, b) => a + b, 0)).toBe(iris.length);
  });

  it('asks once about an entity named twice', async () => {
    const run = vi.fn(async () => []);
    await locateEntities(['http://x/a', 'http://x/a'], { run, convert: never });
    expect(run).toHaveBeenCalledTimes(1);
    expect((run.mock.calls[0] as unknown as [string])[0].match(/<http:\/\/x\/a>/g)).toHaveLength(1);
  });

  it('keeps the first geometry that can be drawn when an entity has several', async () => {
    const run = async () => [
      row('http://x/a', wkt('POINT(garbage')),
      row('http://x/a', wkt('POINT(1 2)')),
      row('http://x/a', wkt('POINT(3 4)')),
    ];
    const convert = async (b: { value: string }[]) => b.map(() => ({ error: 'unreadable' }));
    const out = await locateEntities(['http://x/a'], { run, convert: convert as never });
    expect(out.found.get('http://x/a')).toEqual([1, 2]);
  });

  it('sends the literals the browser cannot read to the server', async () => {
    const run = async () => [
      row('http://x/a', { type: 'literal', value: '<gml/>', datatype: GML_LITERAL }),
    ];
    const convert = vi.fn(async () => [{ geometry: { type: 'Point', coordinates: [9, 8] } }]);
    const out = await locateEntities(['http://x/a'], { run, convert: convert as never });
    expect(convert).toHaveBeenCalledOnce();
    expect(out.found.get('http://x/a')).toEqual([9, 8]);
  });

  it('reads GeoJSON literals in the browser', async () => {
    const run = async () => [
      row('http://x/a', {
        type: 'literal',
        value: '{"type":"Point","coordinates":[5,6]}',
        datatype: GEOJSON_LITERAL,
      }),
    ];
    const out = await locateEntities(['http://x/a'], { run, convert: never });
    expect(out.found.get('http://x/a')).toEqual([5, 6]);
  });

  it('counts the entities of a batch whose query fails and goes on with the others', async () => {
    let calls = 0;
    const run = async (sparql: string) => {
      calls++;
      if (sparql.includes('/e0>')) throw new Error('timeout');
      return [row('http://x/e200', wkt('POINT(1 1)'))];
    };
    const iris = Array.from({ length: 205 }, (_, i) => `http://x/e${i}`);
    const out = await locateEntities(iris, { run, convert: never });
    expect(calls).toBe(2);
    expect(out.failed).toBe(200);
    expect(out.firstError).toBe('timeout');
    expect(out.found.get('http://x/e200')).toEqual([1, 1]);
  });

  it('asks again for the entities a full answer left out', async () => {
    // A has twelve geometries and takes every row of the first answer, B has one
    const calls: string[][] = [];
    const run = vi.fn(async (sparql: string, limit: number) => {
      const asked = sparql.match(/<http:\/\/x\/[ab]>/g) ?? [];
      calls.push(asked);
      if (asked.length === 2)
        return Array.from({ length: limit }, (_, i) => row('http://x/a', wkt(`POINT(${i} 0)`)));
      return [row('http://x/b', wkt('POINT(5 5)'))];
    });
    const out = await locateEntities(['http://x/a', 'http://x/b'], { run, convert: never });
    expect(calls).toHaveLength(2);
    expect(calls[1]).toEqual(['<http://x/b>']);
    expect(out.found.get('http://x/a')).toEqual([0, 0]);
    expect(out.found.get('http://x/b')).toEqual([5, 5]);
  });

  it('does not ask again when no entity was answered or all were', async () => {
    const run = vi.fn(async (_: string, limit: number) =>
      Array.from({ length: limit }, (_, i) => row('http://x/a', wkt(`POINT(${i} 0)`))),
    );
    await locateEntities(['http://x/a'], { run, convert: never });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it('retries a full answer also when the other workers have gone', async () => {
    // 201 entities make two batches. The small one finishes first, so its worker has gone
    // by the time the large one answers full for its first entity alone.
    const iris = Array.from({ length: LOCATE_BATCH + 1 }, (_, i) => `http://x/e${i}`);
    const run = vi.fn(async (sparql: string, limit: number) => {
      const asked = sparql.match(/<http:\/\/x\/e\d+>/g) ?? [];
      if (asked.length === LOCATE_BATCH) {
        await new Promise((r) => setTimeout(r, 30));
        return Array.from({ length: limit }, (_, i) => row('http://x/e0', wkt(`POINT(${i} 0)`)));
      }
      if (asked.length === 1 && asked[0] === '<http://x/e200>')
        return [row('http://x/e200', wkt('POINT(2 2)'))];
      // the retry: everything the full answer left out
      return asked.map((a) => row(a.slice(1, -1), wkt('POINT(1 1)')));
    });
    const out = await locateEntities(iris, { run, convert: never });
    expect(out.found.size).toBe(iris.length);
    expect(out.found.get('http://x/e199')).toEqual([1, 1]);
  });

  it('counts a retried batch once in its progress', async () => {
    const seen: [number, number][] = [];
    const run = async (sparql: string, limit: number) =>
      sparql.includes('/a>') && sparql.includes('/b>')
        ? Array.from({ length: limit }, (_, i) => row('http://x/a', wkt(`POINT(${i} 0)`)))
        : [];
    await locateEntities(['http://x/a', 'http://x/b'], {
      run,
      convert: never,
      onBatch: (_, done, total) => seen.push([done, total]),
    });
    expect(seen).toEqual([
      [1, 2],
      [2, 2],
    ]);
  });

  it('reports progress after each batch', async () => {
    const seen: [number, number][] = [];
    const iris = Array.from({ length: 450 }, (_, i) => `http://x/e${i}`);
    await locateEntities(iris, {
      run: async () => [],
      convert: never,
      onBatch: (_, done, total) => seen.push([done, total]),
    });
    expect(seen).toHaveLength(3);
    expect(seen.at(-1)).toEqual([450, 450]);
  });

  it('stops when aborted', async () => {
    const ctl = new AbortController();
    const run = vi.fn(async () => {
      ctl.abort();
      return [];
    });
    const iris = Array.from({ length: LOCATE_BATCH * 12 }, (_, i) => `http://x/e${i}`);
    await locateEntities(iris, { run, convert: never, signal: ctl.signal });
    expect(run.mock.calls.length).toBeLessThanOrEqual(LOCATE_IN_FLIGHT);
  });

  it('ignores rows that are not an IRI with a geometry literal', async () => {
    const run = async () => [
      { s: { type: 'bnode', value: 'b0' } as Term, lit: wkt('POINT(1 1)') },
      row('http://x/a', { type: 'literal', value: 'hello' }),
      { s: iri('http://x/a') },
    ];
    const out = await locateEntities(['http://x/a'], { run, convert: never });
    expect(out.found.size).toBe(0);
  });
});
