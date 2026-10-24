import { describe, expect, it } from 'vitest';
import {
  boxed,
  byDistance,
  categories,
  densityCounts,
  inPolygon,
  keepSelected,
  keyOf,
  lassoed,
  MAX_CATEGORIES,
  NONE,
  numericBuckets,
  parsePoints,
  pointsOf,
  positions,
  valuesQuery,
  type Points,
} from './embedding';

/** The binary points format as the server writes it (crates/sparkles-server/src/projection.rs). */
function encode(
  head: Record<string, unknown>,
  pts: { entity: string; x: number; y: number; cls: number; colour?: number }[],
  withColour: boolean,
  version = 1,
): ArrayBuffer {
  const enc = new TextEncoder();
  const terms = enc.encode(pts.map((p) => p.entity).join('\n'));
  const hj = enc.encode(
    JSON.stringify({
      ...head,
      count: pts.length,
      termsBytes: terms.length,
      hasColour: withColour,
    }),
  );
  let at = 12 + hj.length + terms.length;
  at = Math.ceil(at / 4) * 4;
  const n = pts.length;
  const buf = new ArrayBuffer(at + n * 12 + (withColour ? n * 4 : 0));
  const u8 = new Uint8Array(buf);
  u8.set([0x53, 0x50, 0x4b, 0x50, 0x54, 0x53, 0x00, version]);
  const dv = new DataView(buf);
  dv.setUint32(8, hj.length, true);
  u8.set(hj, 12);
  u8.set(terms, 12 + hj.length);
  pts.forEach((p, i) => {
    dv.setFloat32(at + i * 8, p.x, true);
    dv.setFloat32(at + i * 8 + 4, p.y, true);
    dv.setUint32(at + n * 8 + i * 4, p.cls, true);
    if (withColour) dv.setUint32(at + n * 12 + i * 4, p.colour ?? NONE, true);
  });
  return buf;
}

const grid = (n: number): Points =>
  pointsOf(
    Array.from({ length: n * n }, (_, i) => ({
      entity: `http://ex/p${i}`,
      x: i % n,
      y: Math.floor(i / n),
    })),
    false,
  );

describe('parsePoints', () => {
  it('reads the columns, the classes and the colours', () => {
    const pts = [
      { entity: 'http://ex/a', x: 1.5, y: -2, cls: 0, colour: 1 },
      { entity: '_:b0', x: 0, y: 3.25, cls: NONE },
      { entity: 'http://ex/ü', x: -1, y: 0.5, cls: 1, colour: 0 },
    ];
    const p = parsePoints(encode({ index: 'e', entities: 3 }, pts, true));
    expect(p.head.index).toBe('e');
    expect(p.points.entities).toEqual(['http://ex/a', '_:b0', 'http://ex/ü']);
    expect([...p.points.x]).toEqual([1.5, 0, -1]);
    expect([...p.points.y]).toEqual([-2, 3.25, 0.5]);
    expect([...p.points.classes]).toEqual([0, NONE, 1]);
    expect([...p.points.colours!]).toEqual([1, NONE, 0]);
  });

  it('reads an empty answer and one without colours', () => {
    expect(parsePoints(encode({}, [], false)).points.entities).toEqual([]);
    const p = parsePoints(encode({}, [{ entity: 'http://ex/a', x: 1, y: 2, cls: 0 }], false));
    expect(p.points.colours).toBeNull();
  });

  it('refuses another version and another format', () => {
    expect(() => parsePoints(encode({}, [], false, 2))).toThrow(/version 2/);
    expect(() => parsePoints(new TextEncoder().encode('{"error":"x"}').buffer)).toThrow(
      /did not send the points format/,
    );
  });
});

describe('selection shapes', () => {
  it('finds the points inside a lasso by the even-odd rule', () => {
    const tri = [
      { x: 0, y: 0 },
      { x: 4, y: 0 },
      { x: 0, y: 4 },
    ];
    expect(inPolygon({ x: 1, y: 1 }, tri)).toBe(true);
    expect(inPolygon({ x: 3, y: 3 }, tri)).toBe(false);
    const p = grid(5);
    const got = lassoed(p, tri).map((i) => [p.x[i], p.y[i]]);
    expect(got).toContainEqual([1, 1]);
    expect(got).not.toContainEqual([3, 3]);
    expect(lassoed(p, tri.slice(0, 2))).toEqual([]);
  });

  it('finds the points inside a box drawn in any direction', () => {
    const p = grid(5);
    const a = boxed(p, { x: 1, y: 1 }, { x: 2, y: 2 });
    expect(a).toHaveLength(4);
    expect(boxed(p, { x: 2, y: 2 }, { x: 1, y: 1 })).toEqual(a);
  });
});

describe('byDistance', () => {
  it('orders the selection from its centre outward and keeps the unplaced at the end', () => {
    const p = pointsOf(
      [
        { entity: 'http://ex/far', x: 10, y: 0 },
        { entity: 'http://ex/mid', x: 1, y: 0 },
        { entity: 'http://ex/near', x: 0, y: 0 },
        { entity: 'http://ex/other', x: -1, y: 0 },
      ],
      false,
    );
    const { ordered, centre } = byDistance(
      [
        '<http://ex/far>',
        '<http://ex/missing>',
        '<http://ex/mid>',
        '<http://ex/near>',
        '<http://ex/other>',
      ],
      p,
    );
    expect(centre).toEqual({ x: 2.5, y: 0 });
    expect(ordered).toEqual([
      '<http://ex/mid>',
      '<http://ex/near>',
      '<http://ex/other>',
      '<http://ex/far>',
      '<http://ex/missing>',
    ]);
  });

  it('orders by the positions it is given, not only the points drawn', () => {
    // a sampled view draws only `drawn`; the entities fetched for the region sit nearer
    // the centre and must come first
    const drawn = pointsOf(
      [
        { entity: 'http://ex/edge1', x: -10, y: 0 },
        { entity: 'http://ex/edge2', x: 10, y: 0 },
      ],
      false,
    );
    const at = positions(drawn);
    at.set('<http://ex/core>', { x: 0, y: 0.5 });
    const ids = ['<http://ex/edge1>', '<http://ex/edge2>', '<http://ex/core>'];
    expect(byDistance(ids, at).ordered[0]).toBe('<http://ex/core>');
    // without the fetched position the core falls behind the drawn points
    expect(byDistance(ids, drawn).ordered.at(-1)).toBe('<http://ex/core>');
  });

  it('keeps only the positions of selected entities', () => {
    const at = new Map([
      ['<a>', { x: 0, y: 0 }],
      ['<b>', { x: 1, y: 1 }],
    ]);
    keepSelected(at, new Set(['<b>']));
    expect([...at.keys()]).toEqual(['<b>']);
    keepSelected(at, new Set());
    expect(at.size).toBe(0);
  });

  it('keys IRIs and blank nodes as the selection does', () => {
    expect(keyOf('http://ex/a')).toBe('<http://ex/a>');
    expect(keyOf('_:b1')).toBe('_:b1');
  });
});

describe('colours', () => {
  it('names the largest categories and folds the rest into other', () => {
    const counts = Array.from({ length: MAX_CATEGORIES + 2 }, (_, i) => ({
      key: `c${i}`,
      label: `c${i}`,
      count: i + 1,
    }));
    const { list, index } = categories(counts, 12);
    expect(list).toHaveLength(MAX_CATEGORIES + 1);
    expect(list[0]).toMatchObject({ key: `c${MAX_CATEGORIES + 1}`, colour: 0 });
    expect(list.at(-1)).toMatchObject({ label: 'other', count: 1 + 2, colour: 12 });
    expect(index[0]).toBe(12);
    expect(index[MAX_CATEGORIES + 1]).toBe(0);
  });

  it('buckets many numbers into ranges and leaves few or non-numbers alone', () => {
    const lit = (v: string) => ({ type: 'literal' as const, value: v });
    const many = Array.from({ length: 20 }, (_, i) => ({ value: lit(String(i)), count: 1 }));
    const b = numericBuckets(many)!;
    expect(b.breaks).toEqual([4, 8, 12, 16]);
    expect(b.of(lit('0'))).toBe(0);
    expect(b.of(lit('19'))).toBe(4);
    expect(numericBuckets(many.slice(0, 3))).toBeNull();
    expect(numericBuckets([...many, { value: lit('x'), count: 1 }])).toBeNull();
  });

  it('reads the density grid', () => {
    const c = new Uint32Array([0, 1, 70000, 0xffffffff]);
    const b64 = btoa(String.fromCharCode(...new Uint8Array(c.buffer)));
    expect([...densityCounts({ size: 2, bbox: [0, 0, 1, 1], max: 0, counts: b64 })]).toEqual([
      0, 1, 70000, 0xffffffff,
    ]);
  });
});

describe('valuesQuery', () => {
  it('writes the entities as a VALUES block with their labels', () => {
    const q = valuesQuery(['<http://ex/a>', '<http://ex/b>']);
    expect(q).toContain('VALUES ?entity {\n    <http://ex/a> <http://ex/b>\n  }');
    expect(q).toContain('OPTIONAL { ?entity rdfs:label ?label }');
  });
});
