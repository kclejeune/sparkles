import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  announcement,
  badgeResult,
  categoryOrder,
  colourMemory,
  colourResult,
  describe as describeLens,
  encode,
  entryIds,
  inferredResult,
  LENS_TOKENS,
  NO_VALUE,
  OTHER,
  PALETTE_SIZE,
  quantileBreaks,
  sizeFactor,
  sizeResult,
  sizeStep,
  stableIndex,
} from './lens';

const at = { at: 'head', branch: 'main' };

describe('quantile steps', () => {
  it('puts the smallest values in step 0 and the largest in step 4', () => {
    const xs = Array.from({ length: 100 }, (_, i) => i + 1);
    const b = quantileBreaks(xs);
    expect(b).toEqual([21, 41, 61, 81]);
    expect(sizeStep(1, b)).toBe(0);
    expect(sizeStep(21, b)).toBe(0);
    expect(sizeStep(22, b)).toBe(1);
    expect(sizeStep(100, b)).toBe(4);
  });

  it('keeps one outlier from flattening the rest', () => {
    const xs = [1, 2, 3, 4, 5, 6, 7, 8, 9, 1000];
    const b = quantileBreaks(xs);
    const steps = xs.map((x) => sizeStep(x, b));
    // the ordinary values spread over the steps, and the outlier is simply the largest
    expect(new Set(steps.slice(0, 9)).size).toBeGreaterThanOrEqual(4);
    expect(steps[9]).toBe(4);
  });

  it('spreads few distinct values over the extremes', () => {
    const xs = [1, 1, 1, 1, 1, 5];
    const b = quantileBreaks(xs);
    expect(b).toEqual([1]);
    expect(sizeStep(1, b)).toBe(0);
    expect(sizeStep(5, b)).toBe(4);
  });

  it('draws larger steps with a larger area', () => {
    expect(sizeFactor(NO_VALUE)).toBe(1);
    expect(sizeFactor(0)).toBe(1);
    for (let s = 1; s < 5; s++) expect(sizeFactor(s)).toBeGreaterThan(sizeFactor(s - 1));
  });
});

describe('sizeResult', () => {
  it('gives each step its range, and counts nodes without a value as "no value"', () => {
    const values = new Map<string, number | null>([
      ['a', 1],
      ['b', 1],
      ['c', 9],
      ['d', null],
    ]);
    const r = sizeResult({ lens: 'degree', name: 'degree', values, computedAt: at });
    expect(r.title).toBe('size is degree, larger is higher');
    expect(r.codes.get('c')).toBe(4);
    expect(r.codes.get('a')).toBe(0);
    expect(r.codes.get('d')).toBe(NO_VALUE);
    expect(r.legend.map((e) => [e.text, e.count])).toEqual([
      ['9', 1],
      ['1', 2],
      ['no value', 1],
    ]);
    expect(r.legend.find((e) => e.key === 'none')?.ids).toEqual(['d']);
  });

  it('gives a single distinct value the middle step', () => {
    const r = sizeResult({
      lens: 'p',
      name: 'p',
      values: new Map([
        ['a', 3],
        ['b', 3],
      ]),
      computedAt: at,
    });
    expect([...r.codes.values()]).toEqual([2, 2]);
  });
});

describe('colourResult', () => {
  it('orders categories by size and gives the thirteenth on "other"', () => {
    const values = new Map<string, string | null>();
    // category k has 20 − k nodes
    for (let k = 0; k < 14; k++)
      for (let i = 0; i < 20 - k; i++) values.set(`n${k}-${i}`, `cat${k}`);
    values.set('none', null);
    const r = colourResult({ lens: 'class', name: 'class', values, computedAt: at });
    expect(r.codes.get('n0-0')).toBe(0);
    expect(r.codes.get('n11-0')).toBe(11);
    expect(r.codes.get('n12-0')).toBe(OTHER);
    expect(r.codes.get('n13-0')).toBe(OTHER);
    expect(r.codes.has('none')).toBe(false);
    const other = r.legend.find((e) => e.key === 'other')!;
    expect(other.text).toBe('other, 2 categories');
    expect(other.count).toBe(8 + 7);
    expect(r.legend.at(-1)).toMatchObject({ key: 'none', text: 'no value', count: 1 });
    expect(r.summary).toBe('14 categories');
  });

  it('breaks ties between equal categories by label', () => {
    const { order } = categoryOrder(
      new Map([
        ['a', 'z'],
        ['b', 'y'],
      ]),
      (v) => ({ z: 'Alpha', y: 'Beta' })[v]!,
    );
    expect(order).toEqual(['z', 'y']);
  });
});

describe('entryIds', () => {
  it('gives the nodes of a legend entry while it exists, and null after (review 8)', () => {
    const colour = colourResult({
      lens: 'class',
      name: 'class',
      values: new Map([
        ['a', 'Person'],
        ['b', 'Person'],
      ]),
      computedAt: at,
    });
    expect(entryIds({ colour }, 'colour:cat-Person')).toEqual(['a', 'b']);
    expect(entryIds({ colour }, 'colour:cat-Robot')).toBeNull();
    expect(entryIds({}, 'colour:cat-Person')).toBeNull();
    expect(entryIds({ colour }, null)).toBeNull();
  });
});

describe('encode and describe', () => {
  it('builds one look per node and the set of inferred edges', () => {
    const size = sizeResult({
      lens: 'degree',
      name: 'degree',
      values: new Map([
        ['a', 1],
        ['b', 5],
      ]),
      computedAt: at,
    });
    const colour = colourResult({
      lens: 'class',
      name: 'class',
      values: new Map([
        ['a', 'Person'],
        ['b', null],
      ]),
      computedAt: at,
    });
    const badge = badgeResult({
      lens: 'inferred',
      name: 'inferred edges',
      values: new Map([
        ['a', 2],
        ['b', 0],
      ]),
      computedAt: at,
    });
    const edge = inferredResult({
      lens: 'inferred',
      name: 'inferred',
      edges: [
        { id: 'e1', source: 'a', target: 'b' },
        { id: 'e2', source: 'b', target: 'a' },
        { id: 'e3', source: 'a', target: '_:x' },
      ],
      inferred: new Set(['e1']),
      unchecked: new Set(['e3']),
      computedAt: at,
    });
    const enc = encode({ size, colour, badge, edge });
    expect(enc.nodes.get('a')).toEqual({ size: 0, colour: 0, badge: 2 });
    expect(enc.nodes.get('b')).toEqual({ size: 4 });
    expect([...enc.inferred]).toEqual(['e1']);
    expect(enc.edge && enc.size && enc.colour && enc.badge).toBe(true);
    expect(edge.summary).toBe('1 of 3 edges inferred');
    expect(edge.legend.map((e) => e.key)).toEqual(['inferred', 'unchecked']);
    expect(describeLens('a', { size, colour, badge })).toBe(
      'degree 1, class Person, inferred edges 2',
    );
    expect(encode({})).toMatchObject({ size: false, colour: false, badge: false, edge: false });
  });

  it('announces the top node of a size lens', () => {
    const r = sizeResult({
      lens: 'degree',
      name: 'degree',
      values: new Map([
        ['a', 1],
        ['b', 81],
      ]),
      computedAt: at,
    });
    expect(announcement(r, 'degree', (id) => `ex:${id}`)).toBe(
      'Size by degree on 2 nodes, top node ex:b with 81',
    );
  });
});

/** WCAG relative luminance of `#rrggbb`. */
function luminance(hex: string): number {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const [r, g, b] = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
const contrast = (a: string, b: string) => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
};

describe('the lens palette', () => {
  const css = readFileSync(resolve(import.meta.dirname, '../app.css'), 'utf8');
  /** The custom properties of each block that sets `--surface`: light, dark by media, dark by attribute. */
  const blocks = css
    .split('}')
    .filter((b) => /--surface:\s*#/.test(b))
    .map((b) =>
      Object.fromEntries(
        [...b.matchAll(/(--[\w-]+):\s*(#[0-9a-f]{6})/gi)].map((m) => [m[1], m[2]]),
      ),
    );

  it('is defined for the light theme and both dark ones', () => {
    expect(blocks.length).toBe(3);
    for (const b of blocks) for (const t of LENS_TOKENS) expect(b[t], t).toMatch(/^#[0-9a-f]{6}$/i);
    expect(LENS_TOKENS.length).toBe(PALETTE_SIZE + 1);
  });

  it('has at least 3:1 contrast against the surface in each theme', () => {
    for (const b of blocks)
      for (const t of LENS_TOKENS)
        expect(contrast(b[t], b['--surface']), `${t} on ${b['--surface']}`).toBeGreaterThanOrEqual(
          3,
        );
  });

  it('has twelve different colours in each theme', () => {
    for (const b of blocks)
      expect(new Set(LENS_TOKENS.map((t) => b[t])).size).toBe(PALETTE_SIZE + 1);
  });
});

describe('stable community colours (§2.3)', () => {
  const vals = (o: Record<string, string>) => new Map<string, string | null>(Object.entries(o));

  it('keeps the colour of a community that overlaps half or more of an earlier one', () => {
    const first = colourResult({
      lens: 'leiden',
      name: 'Leiden',
      values: vals({ a: 'x', b: 'x', c: 'x', d: 'y', e: 'y', f: 'z' }),
      computedAt: at,
    });
    const before = colourMemory(first);
    expect(before.index.get('x')).toBe(0);
    expect(before.index.get('y')).toBe(1);
    // the next run renames the communities, and its biggest one is the old y
    const next = vals({ a: '7', b: '7', d: '3', e: '3', g: '3', f: '9' });
    const index = stableIndex(next, before);
    // '3' is the largest now but keeps y's colour, '7' keeps two of x's three members
    expect(index.get('3')).toBe(1);
    expect(index.get('7')).toBe(0);
    expect(index.get('9')).toBe(2);
    const r = colourResult({ lens: 'leiden', name: 'Leiden', values: next, index, computedAt: at });
    // the legend lists the colours in palette order
    expect(r.legend.map((e) => e.swatch)).toEqual([0, 1, 2]);
    expect(r.codes.get('d')).toBe(1);
  });

  it('gives a new colour to a community below half the overlap, and each old colour once', () => {
    const before = {
      members: new Map([['x', new Set(['a', 'b', 'c', 'd'])]]),
      index: new Map([['x', 0]]),
    };
    // one member of four is below half: a fresh colour
    expect(stableIndex(vals({ a: 'p', e: 'p', f: 'p' }), before).get('p')).toBe(0);
    // two communities each with half of x: the larger takes it, the other a free colour
    const i = stableIndex(vals({ a: 'p', b: 'p', z: 'p', c: 'q', d: 'q' }), before);
    expect(i.get('p')).toBe(0);
    expect(i.get('q')).toBe(1);
  });

  it('picks the largest overlap among the earlier communities that qualify', () => {
    const xs = Array.from({ length: 10 }, (_, i) => `x${i}`);
    const ys = ['y0', 'y1', 'y2'];
    const before = {
      members: new Map([
        ['X', new Set(xs)],
        ['Y', new Set(ys)],
      ]),
      index: new Map([
        ['X', 0],
        ['Y', 1],
      ]),
    };
    const o: Record<string, string> = {};
    // n holds four of X's ten, below half, and all three of Y
    for (const id of [...xs.slice(0, 4), ...ys]) o[id] = 'n';
    for (const id of xs.slice(4)) o[id] = 'm';
    // a larger new community that would take a freed colour first
    for (let i = 0; i < 8; i++) o[`new${i}`] = 'l';
    const index = stableIndex(vals(o), before);
    expect(index.get('n')).toBe(1);
    expect(index.get('m')).toBe(0);
    expect(index.get('l')).toBe(2);
  });

  it('sends communities beyond the palette to "other"', () => {
    const o: Record<string, string> = {};
    for (let i = 0; i < 15; i++) o[`n${i}`] = `c${i}`;
    const index = stableIndex(vals(o), null);
    expect([...index.values()].filter((v) => v === OTHER)).toHaveLength(3);
    const r = colourResult({
      lens: 'wcc',
      name: 'WCC',
      values: vals(o),
      index,
      noun: ['component', 'components'],
      entry: (v, n) => `component ${v}, ${n} node`,
      computedAt: at,
    });
    expect(r.legend.at(-1)?.text).toBe('other, 3 components');
    expect(r.legend[0].text).toMatch(/^component c\d+, 1 node$/);
    expect(r.summary).toBe('15 components');
  });
});
