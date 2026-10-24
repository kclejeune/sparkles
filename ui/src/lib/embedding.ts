// The embedding map's data (docs/specs/U11-linked-views.md, section 2.4): the projection
// route's binary points format, the density grid, the lasso and box selections, the
// colour categories and the order in which a large selection becomes a VALUES block.

import type { Term } from './api';

/** The binary points format's magic and version; any other is refused. */
export const POINTS_MAGIC = [0x53, 0x50, 0x4b, 0x50, 0x54, 0x53, 0x00, 0x01];
export const POINTS_TYPE = 'application/x-sparkles-points';
/** No class or no colour value. */
export const NONE = 0xffffffff;
/** The most entities a VALUES block takes. */
export const VALUES_CAP = 1000;
/** The categories a legend names before "other". */
export const MAX_CATEGORIES = 8;

export type ProjectionHead = {
  index: string;
  method: 'pca' | 'neighbours';
  commit: number;
  generation?: string;
  stale: boolean;
  vectors: number;
  dimension: number;
  entities: number;
  bounds: [number, number, number, number];
  explained?: [number, number];
  builtMs?: number;
  bytes?: number;
  region: {
    bbox: [number, number, number, number];
    total: number;
    shown: number;
    sampled: boolean;
  };
  density: { size: number; bbox: [number, number, number, number]; max: number; counts: string };
  classes: { iri: string; count: number }[];
  colour?: { predicate: string; values: { value: Term; count: number }[] };
};

/** The points of an answer, in columns. */
export type Points = {
  entities: string[];
  x: Float32Array;
  y: Float32Array;
  /** an index into `classes`, or NONE */
  classes: Uint32Array;
  /** an index into `colour.values`, or NONE; absent without `colour=` */
  colours: Uint32Array | null;
};

export type Projection = { head: ProjectionHead; points: Points };

/** Read the binary points format, or throw on another format or version. */
export function parsePoints(buf: ArrayBuffer): Projection {
  const bytes = new Uint8Array(buf);
  if (bytes.length < 12 || POINTS_MAGIC.some((b, i) => bytes[i] !== b))
    throw new Error(
      bytes.length >= 8 && bytes.slice(0, 6).every((b, i) => b === POINTS_MAGIC[i])
        ? `The server sent version ${bytes[7]} of the points format, which this UI cannot read. Update the UI.`
        : 'The server did not send the points format',
    );
  const view = new DataView(buf);
  const hl = view.getUint32(8, true);
  const head = JSON.parse(
    new TextDecoder().decode(bytes.subarray(12, 12 + hl)),
  ) as ProjectionHead & {
    count: number;
    termsBytes: number;
    hasColour: boolean;
  };
  const n = head.count;
  let at = 12 + hl;
  const text = new TextDecoder().decode(bytes.subarray(at, at + head.termsBytes));
  const entities = n ? text.split('\n') : [];
  at += head.termsBytes;
  at = Math.ceil(at / 4) * 4;
  const x = new Float32Array(n);
  const y = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    x[i] = view.getFloat32(at + i * 8, true);
    y[i] = view.getFloat32(at + i * 8 + 4, true);
  }
  at += n * 8;
  const classes = new Uint32Array(n);
  for (let i = 0; i < n; i++) classes[i] = view.getUint32(at + i * 4, true);
  at += n * 4;
  let colours: Uint32Array | null = null;
  if (head.hasColour) {
    colours = new Uint32Array(n);
    for (let i = 0; i < n; i++) colours[i] = view.getUint32(at + i * 4, true);
  }
  return { head, points: { entities, x, y, classes, colours } };
}

/** The JSON answer's points in columns. */
export function pointsOf(
  list: { entity: string; x: number; y: number; class?: number; colour?: number }[],
  withColour: boolean,
): Points {
  const n = list.length;
  const p: Points = {
    entities: list.map((q) => q.entity),
    x: new Float32Array(n),
    y: new Float32Array(n),
    classes: new Uint32Array(n),
    colours: withColour ? new Uint32Array(n) : null,
  };
  list.forEach((q, i) => {
    p.x[i] = q.x;
    p.y[i] = q.y;
    p.classes[i] = q.class ?? NONE;
    if (p.colours) p.colours[i] = q.colour ?? NONE;
  });
  return p;
}

/** The density grid's counts. */
export function densityCounts(d: ProjectionHead['density']): Uint32Array {
  const bin = atob(d.counts);
  const out = new Uint32Array(bin.length / 4);
  for (let i = 0; i < out.length; i++)
    out[i] =
      bin.charCodeAt(i * 4) |
      (bin.charCodeAt(i * 4 + 1) << 8) |
      (bin.charCodeAt(i * 4 + 2) << 16) |
      ((bin.charCodeAt(i * 4 + 3) << 24) >>> 0);
  return out;
}

/** The selection's key of an entity of an answer: `<iri>`, or `_:label`. */
export function keyOf(entity: string): string {
  return entity.startsWith('_:') ? entity : `<${entity}>`;
}

export type Pt = { x: number; y: number };

/** Whether `p` lies inside the polygon `poly` (even-odd rule). */
export function inPolygon(p: Pt, poly: readonly Pt[]): boolean {
  let inside = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const a = poly[i];
    const b = poly[j];
    if (a.y > p.y !== b.y > p.y && p.x < ((b.x - a.x) * (p.y - a.y)) / (b.y - a.y) + a.x)
      inside = !inside;
  }
  return inside;
}

/** The points inside a lasso, given in the points' own coordinates. */
export function lassoed(points: Points, poly: readonly Pt[]): number[] {
  if (poly.length < 3) return [];
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const q of poly) {
    x0 = Math.min(x0, q.x);
    y0 = Math.min(y0, q.y);
    x1 = Math.max(x1, q.x);
    y1 = Math.max(y1, q.y);
  }
  const out: number[] = [];
  for (let i = 0; i < points.x.length; i++) {
    const x = points.x[i];
    const y = points.y[i];
    if (x < x0 || x > x1 || y < y0 || y > y1) continue;
    if (inPolygon({ x, y }, poly)) out.push(i);
  }
  return out;
}

/** The points inside a box with corners `a` and `b`. */
export function boxed(points: Points, a: Pt, b: Pt): number[] {
  const [x0, x1] = [Math.min(a.x, b.x), Math.max(a.x, b.x)];
  const [y0, y1] = [Math.min(a.y, b.y), Math.max(a.y, b.y)];
  const out: number[] = [];
  for (let i = 0; i < points.x.length; i++)
    if (points.x[i] >= x0 && points.x[i] <= x1 && points.y[i] >= y0 && points.y[i] <= y1)
      out.push(i);
  return out;
}

/** The position of each entity of `points`, by term key. */
export function positions(points: Points, into = new Map<string, Pt>()): Map<string, Pt> {
  points.entities.forEach((e, i) => into.set(keyOf(e), { x: points.x[i], y: points.y[i] }));
  return into;
}

/**
 * The selected entities (term keys) in the order a VALUES block takes them: by distance
 * from the selection's centre, nearest first, so that a selection above the cap keeps its
 * core. `at` gives the positions known, as points or by key. Entities without a position
 * keep their order after those with one.
 */
export function byDistance(
  selected: readonly string[],
  at: Points | ReadonlyMap<string, Pt>,
): { ordered: string[]; centre: Pt | null } {
  const where = at instanceof Map ? at : positions(at as Points);
  const placed = selected.filter((k) => where.has(k));
  if (!placed.length) return { ordered: [...selected], centre: null };
  let cx = 0;
  let cy = 0;
  for (const k of placed) {
    const p = where.get(k)!;
    cx += p.x;
    cy += p.y;
  }
  cx /= placed.length;
  cy /= placed.length;
  const dist = new Map<string, number>();
  for (const k of placed) {
    const p = where.get(k)!;
    dist.set(k, (p.x - cx) ** 2 + (p.y - cy) ** 2);
  }
  const ordered = [...placed].sort(
    (a, b) => dist.get(a)! - dist.get(b)! || (a < b ? -1 : a > b ? 1 : 0),
  );
  return {
    ordered: [...ordered, ...selected.filter((k) => !where.has(k))],
    centre: { x: cx, y: cy },
  };
}

export type Category = { key: string; label: string; count: number; colour: number };

/**
 * The legend of a colouring: the `MAX_CATEGORIES` largest categories, each with a palette
 * index, and "other" for the rest. `index` maps each source index (a class or a colour
 * value) to its palette index.
 */
export function categories(
  counts: readonly { key: string; label: string; count: number }[],
  other: number,
): { list: Category[]; index: Uint32Array } {
  const order = counts
    .map((c, i) => ({ ...c, i }))
    .sort((a, b) => b.count - a.count || (a.label < b.label ? -1 : 1));
  const index = new Uint32Array(counts.length).fill(other);
  const list: Category[] = [];
  let rest = 0;
  order.forEach((c, k) => {
    if (k < MAX_CATEGORIES) {
      index[c.i] = k;
      list.push({ key: c.key, label: c.label, count: c.count, colour: k });
    } else rest += c.count;
  });
  if (rest) list.push({ key: '', label: 'other', count: rest, colour: other });
  return { list, index };
}

/** Numeric colour values in up to five ranges of equal count, or null if any is not a number. */
export function numericBuckets(values: readonly { value: Term; count: number }[]): {
  breaks: number[];
  of: (v: Term) => number;
} | null {
  const nums = values.map((v) => (v.value.type === 'literal' ? Number(v.value.value) : NaN));
  if (!nums.length || nums.some((n) => !Number.isFinite(n))) return null;
  if (new Set(nums).size <= MAX_CATEGORIES) return null;
  const flat: number[] = [];
  values.forEach((v, i) => {
    for (let k = 0; k < Math.min(v.count, 1000); k++) flat.push(nums[i]);
  });
  flat.sort((a, b) => a - b);
  const breaks: number[] = [];
  for (let k = 1; k < 5; k++) {
    const b = flat[Math.floor((k * flat.length) / 5)];
    if (breaks[breaks.length - 1] !== b) breaks.push(b);
  }
  const of = (t: Term) => {
    const n = Number(t.value);
    const k = breaks.findIndex((b) => n < b);
    return k < 0 ? breaks.length : k;
  };
  return { breaks, of };
}

/** The query **Use in query** opens: the entities and their labels. */
export function valuesQuery(ids: readonly string[]): string {
  const lines: string[] = [];
  for (let i = 0; i < ids.length; i += 4) lines.push(`    ${ids.slice(i, i + 4).join(' ')}`);
  return `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?entity ?label WHERE {
  VALUES ?entity {
${lines.join('\n')}
  }
  OPTIONAL { ?entity rdfs:label ?label }
}
`;
}
