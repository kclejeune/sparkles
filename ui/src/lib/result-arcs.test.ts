import { describe, expect, it } from 'vitest';
import type { AtInfo, Term } from './api';
import { MAX_ARCS } from './arcs';
import { GEOJSON_LITERAL, WKT_LITERAL } from './geo';
import {
  arcColumns,
  defaultArcColumns,
  endpointId,
  entityColumns,
  locateEnds,
  locateEndsCompared,
  readAt,
  resultLinks,
} from './result-arcs';

const iri = (value: string): Term => ({ type: 'uri', value });
const wkt = (value: string): Term => ({ type: 'literal', value, datatype: WKT_LITERAL });
const str = (value: string): Term => ({ type: 'literal', value });
const never = async () => {
  throw new Error('convert should not run');
};
const prefixes = {};
const lit = (term: Term) => (term.type === 'literal' ? term.value : '');
void lit;

describe('columns for arcs', () => {
  const vars = ['from', 'to', 'name'];
  const geoRows = [[wkt('POINT(0 0)'), wkt('POINT(1 1)'), str('a')]];
  const entityRows = [[iri('http://x/a'), iri('http://x/b'), str('a')]];

  it('takes the first two geometry columns', () => {
    expect(defaultArcColumns(vars, geoRows)).toEqual({ from: 'from', to: 'to' });
    expect(arcColumns(vars, geoRows)).toEqual(['from', 'to']);
  });

  it('takes the first two entity columns when there are no geometry columns', () => {
    expect(entityColumns(vars, entityRows)).toEqual(['from', 'to']);
    expect(defaultArcColumns(vars, entityRows)).toEqual({ from: 'from', to: 'to' });
  });

  it('has none for a result with one geometry column or one entity column', () => {
    expect(defaultArcColumns(['a', 'b'], [[wkt('POINT(0 0)'), str('x')]])).toBeNull();
    expect(defaultArcColumns(['a', 'b'], [[iri('http://x/a'), str('x')]])).toBeNull();
    expect(defaultArcColumns([], [])).toBeNull();
  });

  it('lists geometry columns before entity columns', () => {
    const rows = [[iri('http://x/a'), wkt('POINT(0 0)')]];
    expect(arcColumns(['e', 'g'], rows)).toEqual(['g', 'e']);
  });

  it('does not count a column that mixes geometries and IRIs as an entity column', () => {
    const rows = [[iri('http://x/a')], [wkt('POINT(0 0)')]];
    expect(entityColumns(['x'], rows)).toEqual([]);
  });

  it('names the ends by IRI or by geometry, and nothing else', () => {
    expect(endpointId(iri('http://x/a'))).toBe('<http://x/a>');
    expect(endpointId(wkt('POINT(0 0)'))).toContain('POINT(0 0)');
    expect(endpointId(str('hello'))).toBeNull();
    expect(endpointId(null)).toBeNull();
    expect(endpointId(wkt('POINT(0 0)'))).toBe(endpointId(wkt('POINT(0 0)')));
  });
});

describe('links from rows', () => {
  const vars = ['a', 'b', 'label'];
  const rows: (Term | null)[][] = [
    [iri('http://x/lyon'), iri('http://x/paris'), str('train')],
    [iri('http://x/paris'), null, str('nothing')],
    [iri('http://x/lyon'), iri('http://x/paris'), str('bus')],
  ];

  it('draws one link per row, from the first column to the second', () => {
    const r = resultLinks(vars, rows, { from: 'a', to: 'b', prefixes });
    expect(r.links.map((l) => [l.from, l.to, l.row])).toEqual([
      ['<http://x/lyon>', '<http://x/paris>', 0],
      ['<http://x/lyon>', '<http://x/paris>', 2],
    ]);
    // a row without a second end gives no link
    expect(r.skipped).toBe(1);
    expect(r.ends.size).toBe(2);
    expect(r.links[0].predicate).toBe('?a → ?b');
  });

  it('labels the links by a third column', () => {
    const r = resultLinks(vars, rows, { from: 'a', to: 'b', label: 'label', prefixes });
    expect(r.links.map((l) => l.label)).toEqual(['train', 'bus']);
  });

  it('marks the links of added and removed rows', () => {
    const r = resultLinks(vars, rows, {
      from: 'a',
      to: 'b',
      prefixes,
      marks: ['added', 'kept', 'removed'],
    });
    expect(r.links.map((l) => l.change)).toEqual(['added', 'removed']);
  });

  it('reads at most the first MAX_ARCS rows', () => {
    const many = Array.from({ length: MAX_ARCS + 3 }, () => [iri('http://x/a'), iri('http://x/b')]);
    const r = resultLinks(['a', 'b'], many, { from: 'a', to: 'b', prefixes });
    expect(r.links).toHaveLength(MAX_ARCS);
    expect(r.dropped).toBe(3);
  });

  it('draws nothing for columns that do not exist', () => {
    expect(resultLinks(vars, rows, { from: 'nope', to: 'b', prefixes }).links).toEqual([]);
  });

  it('names geometry ends by a short text of the literal', () => {
    const r = resultLinks(['f', 't'], [[wkt('POINT(1 2)'), wkt('POINT(3 4)')]], {
      from: 'f',
      to: 't',
      prefixes,
    });
    expect([...r.ends.values()].map((e) => e.label)).toEqual(['POINT(1 2)', 'POINT(3 4)']);
  });
});

describe('positions of the ends', () => {
  it('reads geometry literals and looks entities up', async () => {
    const r = resultLinks(
      ['f', 't'],
      [
        [wkt('POINT(1 2)'), iri('http://x/b')],
        [wkt('POINT(garbage'), iri('http://x/c')],
      ],
      { from: 'f', to: 't', prefixes },
    );
    const found = await locateEnds(r.ends, {
      run: async () => [
        { s: iri('http://x/b'), lit: wkt('POINT(5 6)') },
        {
          s: iri('http://x/c'),
          lit: {
            type: 'literal',
            value: '{"type":"Point","coordinates":[7,8]}',
            datatype: GEOJSON_LITERAL,
          },
        },
      ],
      convert: async (b) => b.map(() => ({ error: 'no' })),
    });
    expect(found.places.map((p) => [p.id, p.lon, p.lat])).toEqual([
      [expect.stringContaining('POINT(1 2)'), 1, 2],
      ['<http://x/b>', 5, 6],
      ['<http://x/c>', 7, 8],
    ]);
    // the malformed literal is listed with the reason
    expect(found.missing).toHaveLength(1);
    expect(found.missing[0].why).toBeTruthy();
  });

  it('lists an entity without a geometry as not located', async () => {
    const r = resultLinks(['a', 'b'], [[iri('http://x/a'), iri('http://x/b')]], {
      from: 'a',
      to: 'b',
      prefixes,
    });
    const found = await locateEnds(r.ends, {
      run: async () => [{ s: iri('http://x/a'), lit: wkt('POINT(1 1)') }],
      convert: async () => [],
    });
    expect(found.places.map((p) => p.id)).toEqual(['<http://x/a>']);
    expect(found.missing.map((m) => m.iri)).toEqual(['http://x/b']);
  });

  it('reports failed lookups', async () => {
    const r = resultLinks(['a', 'b'], [[iri('http://x/a'), iri('http://x/b')]], {
      from: 'a',
      to: 'b',
      prefixes,
    });
    const found = await locateEnds(r.ends, {
      run: async () => {
        throw new Error('403 forbidden');
      },
      convert: async () => [],
    });
    expect(found.places).toEqual([]);
    expect(found.failed).toBe(2);
    expect(found.firstError).toBe('403 forbidden');
  });
});

describe('the state a result was read at', () => {
  const info = (over: Partial<AtInfo>): AtInfo => ({
    selector: 'commit:2',
    commit: 2,
    head: 2,
    historical: false,
    datetime: null,
    ...over,
  });

  it('is the head for a read without a state', () => {
    expect(readAt(undefined)).toBe('');
  });

  it('pins a read that named a state to its commit, also when that was the head then', () => {
    expect(readAt(info({}))).toBe('commit:2');
    expect(readAt(info({ selector: 'head', commit: 7, head: 7 }))).toBe('commit:7');
    expect(
      readAt(info({ selector: 'time:2026-01-01T00:00:00Z', commit: 3, historical: true })),
    ).toBe('commit:3');
  });

  it('falls back to the selector when the server named no commit', () => {
    expect(readAt(info({ selector: 'snapshot:s2', commit: null }))).toBe('snapshot:s2');
    expect(readAt(info({ selector: 'head', commit: null }))).toBe('');
  });
});

describe('positions in a comparison', () => {
  it('locates the ends that only removed rows use at the earlier state', async () => {
    const rows: (Term | null)[][] = [
      [iri('http://x/kept'), iri('http://x/org')],
      [iri('http://x/gone'), iri('http://x/org')],
    ];
    const r = resultLinks(['a', 'b'], rows, {
      from: 'a',
      to: 'b',
      prefixes,
      marks: ['kept', 'removed'],
    });
    const now: string[] = [];
    const before: string[] = [];
    const answer = (log: string[], where: Record<string, string>) => async (sparql: string) => {
      const asked = Object.keys(where).filter((k) => sparql.includes(`<http://x/${k}>`));
      log.push(...asked);
      return asked.map((k) => ({ s: iri(`http://x/${k}`), lit: wkt(where[k]) }));
    };
    const found = await locateEndsCompared(
      r.ends,
      r.links,
      {
        run: answer(now, { kept: 'POINT(1 1)', org: 'POINT(2 2)' }),
        convert: never,
      },
      {
        run: answer(before, { gone: 'POINT(3 3)', org: 'POINT(9 9)' }),
        convert: never,
      },
    );
    // the organization has a kept row, so it is located now, and the removed person before
    expect(now.sort()).toEqual(['kept', 'org']);
    expect(before).toEqual(['gone']);
    expect(found.places.map((p) => [p.id, p.lon])).toEqual([
      ['<http://x/kept>', 1],
      ['<http://x/org>', 2],
      ['<http://x/gone>', 3],
    ]);
    expect(found.missing).toEqual([]);
  });

  it('is the plain lookup without an earlier state', async () => {
    const r = resultLinks(['a', 'b'], [[iri('http://x/a'), iri('http://x/b')]], {
      from: 'a',
      to: 'b',
      prefixes,
      marks: ['removed'],
    });
    const found = await locateEndsCompared(r.ends, r.links, {
      run: async () => [{ s: iri('http://x/a'), lit: wkt('POINT(1 1)') }],
      convert: never,
    });
    expect(found.places.map((p) => p.id)).toEqual(['<http://x/a>']);
    expect(found.missing.map((m) => m.iri)).toEqual(['http://x/b']);
  });
});
