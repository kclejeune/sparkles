// Lenses on the graph canvas (docs/specs/U10-canvas-lenses.md §4.1 and §4.2): a lens maps
// data about the drawn elements to how they look, on one of four channels. This module holds
// the model, the scales and the encoding tables that both graph views draw from. It has no
// Svelte and makes no requests: lib/lens-sources.ts reads the data.

export type Channel = 'size' | 'colour' | 'badge' | 'edge';
export const CHANNELS: readonly Channel[] = ['size', 'colour', 'badge', 'edge'];
export const CHANNEL_NAMES: Record<Channel, string> = {
  size: 'Size',
  colour: 'Colour',
  badge: 'Badges',
  edge: 'Edges',
};

export type LensValue = number | string | null;

/** One line of a lens's legend. `ids` are the nodes it stands for, which a click selects. */
export type LegendEntry = {
  key: string;
  text: string;
  count: number;
  ids: string[];
  /** The palette index of a colour entry, `OTHER` for "other". */
  swatch?: number;
  /** The size step of a size entry, `NO_VALUE` for nodes without a value. */
  step?: number;
  /** The GraphRAG community of the entry, whose report the legend links to. */
  community?: string;
};

export type LensResult = {
  channel: Channel;
  /** The lens's id, such as `degree`. */
  lens: string;
  /** The lens's name, such as "degree in the drawn graph". */
  name: string;
  /** What the lens shows, such as "size is degree in the drawn graph, larger is higher". */
  title: string;
  /** Element id to its value. Node ids for size, colour and badge, edge ids for edge. */
  values: Map<string, LensValue>;
  /** Element id to what the canvas draws: a size step, a palette index, a count or 1. */
  codes: Map<string, number>;
  /** Element id to its value as text, for the list column and descriptions. */
  text: Map<string, string>;
  legend: LegendEntry[];
  summary?: string;
  /** The levels of a hierarchical run, for its level picker. */
  levels?: number[];
  computedAt: { at: string; branch: string };
};

/** Size steps (§4.2): five quantiles of the drawn values. */
export const SIZE_STEPS = 5;
/** The step of a node without a value, drawn at the smallest size. */
export const NO_VALUE = -1;
/** Node area of each step, relative to the smallest. Area, not radius, carries the value. */
export const STEP_AREA = [1, 2, 3.5, 5.5, 8] as const;

/** The radius factor of a size step: the square root of its area. */
export function sizeFactor(step: number | undefined): number {
  if (step == null || step < 0) return 1;
  return Math.sqrt(STEP_AREA[Math.min(step, SIZE_STEPS - 1)]);
}

/** The palette's qualitative colours (§4.2), CSS tokens beside `--added` and `--removed`. */
export const PALETTE_SIZE = 12;
/** The palette index of "other", for categories beyond the twelfth. */
export const OTHER = PALETTE_SIZE;
export const LENS_TOKENS: readonly string[] = [
  ...Array.from({ length: PALETTE_SIZE }, (_, i) => `--lens-${i + 1}`),
  '--lens-other',
];

/** The palette, read from the CSS tokens of `el` (index `OTHER` is "other"). */
export function readPalette(el: Element): string[] {
  const css = getComputedStyle(el);
  return LENS_TOKENS.map((t) => css.getPropertyValue(t).trim() || '#888888');
}

const fmt = (n: number) =>
  Number.isInteger(n)
    ? n.toLocaleString('en-US')
    : n.toLocaleString('en-US', { maximumSignificantDigits: 4 });

/**
 * The upper bounds of the quantile steps of `values`, without repeats, ascending. A value
 * falls into the step of the first bound it does not exceed, or above the last one.
 */
export function quantileBreaks(values: readonly number[], steps = SIZE_STEPS): number[] {
  const xs = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!xs.length) return [];
  const out: number[] = [];
  for (let k = 1; k < steps; k++) {
    const b = xs[Math.min(xs.length - 1, Math.floor((k * xs.length) / steps))];
    if (out[out.length - 1] !== b) out.push(b);
  }
  // a bound at the largest value would leave the top step empty
  while (out.length && out[out.length - 1] >= xs[xs.length - 1]) out.pop();
  return out;
}

/**
 * The size step, 0 to `SIZE_STEPS - 1`, of a value. The steps the breaks make are spread
 * over the five sizes, so that the smallest values are smallest and the largest largest
 * whatever the number of distinct values. A single distinct value takes the middle step.
 */
export function sizeStep(v: number, breaks: readonly number[]): number {
  if (!breaks.length) return 0;
  let i = 0;
  while (i < breaks.length && v > breaks[i]) i++;
  return Math.round((i * (SIZE_STEPS - 1)) / breaks.length);
}

type Common = { lens: string; name: string; computedAt: LensResult['computedAt'] };

/**
 * A size lens over numeric values (§2.1, §2.7): node area by quantile in five steps, with
 * each step's range in the legend and nodes without a value at the smallest size, counted
 * as "no value".
 */
export function sizeResult(
  c: Common & { values: Map<string, number | null>; unit?: (n: number) => string },
): LensResult {
  const nums = [...c.values.values()].filter((v): v is number => v != null && Number.isFinite(v));
  const breaks = quantileBreaks(nums);
  const single = new Set(nums).size === 1;
  const codes = new Map<string, number>();
  const text = new Map<string, string>();
  const steps = new Map<number, { ids: string[]; min: number; max: number }>();
  const none: string[] = [];
  const unit = c.unit ?? fmt;
  for (const [id, v] of c.values) {
    if (v == null || !Number.isFinite(v)) {
      codes.set(id, NO_VALUE);
      text.set(id, 'no value');
      none.push(id);
      continue;
    }
    const step = single ? 2 : sizeStep(v, breaks);
    codes.set(id, step);
    text.set(id, unit(v));
    const s = steps.get(step);
    if (s) {
      s.ids.push(id);
      s.min = Math.min(s.min, v);
      s.max = Math.max(s.max, v);
    } else steps.set(step, { ids: [id], min: v, max: v });
  }
  const legend: LegendEntry[] = [...steps.entries()]
    .sort(([a], [b]) => b - a)
    .map(([step, s]) => ({
      key: `step-${step}`,
      text: s.min === s.max ? unit(s.min) : `${unit(s.min)} to ${unit(s.max)}`,
      count: s.ids.length,
      ids: s.ids,
      step,
    }));
  if (none.length)
    legend.push({ key: 'none', text: 'no value', count: none.length, ids: none, step: NO_VALUE });
  const values = new Map<string, LensValue>(c.values);
  return {
    channel: 'size',
    lens: c.lens,
    name: c.name,
    title: `size is ${c.name}, larger is higher`,
    values,
    codes,
    text,
    legend,
    computedAt: c.computedAt,
  };
}

/**
 * Categories in palette order: by how many nodes have them, most first, then by their label.
 * The first twelve take the palette's colours and the rest share "other".
 */
export function categoryOrder(
  values: Map<string, string | null>,
  label: (v: string) => string = (v) => v,
): { order: string[]; index: Map<string, number> } {
  const counts = new Map<string, number>();
  for (const v of values.values()) if (v != null) counts.set(v, (counts.get(v) ?? 0) + 1);
  const order = [...counts.keys()].sort(
    (a, b) =>
      counts.get(b)! - counts.get(a)! || label(a).localeCompare(label(b)) || (a < b ? -1 : 1),
  );
  const index = new Map(order.map((v, i) => [v, Math.min(i, OTHER)]));
  return { order, index };
}

/** The communities of a colour lens's last run and their palette indexes (§2.3). */
export type ColourMemory = { members: Map<string, Set<string>>; index: Map<string, number> };

/**
 * Palette indexes that stay stable across runs (§2.3). Communities take colours in order of
 * their size in the drawn graph, and a community whose members overlap half or more of an
 * earlier community's members keeps that one's colour, each earlier colour going to one
 * community at most. The rest take the free colours in order, then "other".
 */
export function stableIndex(
  values: Map<string, string | null>,
  prev: ColourMemory | null | undefined,
  label: (v: string) => string = (v) => v,
): Map<string, number> {
  const { order, index: fresh } = categoryOrder(values, label);
  if (!prev || !prev.members.size) return fresh;
  const before = new Map<string, string>();
  for (const [cat, ids] of prev.members) for (const id of ids) before.set(id, cat);
  const members = new Map<string, string[]>();
  for (const [id, v] of values) {
    if (v == null) continue;
    const l = members.get(v);
    if (l) l.push(id);
    else members.set(v, [id]);
  }
  const out = new Map<string, number>();
  const used = new Set<number>();
  const claimed = new Set<string>();
  for (const cat of order) {
    const overlap = new Map<string, number>();
    for (const id of members.get(cat) ?? []) {
      const p = before.get(id);
      if (p != null && !claimed.has(p)) overlap.set(p, (overlap.get(p) ?? 0) + 1);
    }
    let best: [string, number] | null = null;
    for (const [p, n] of overlap)
      if (!best || n > best[1] || (n === best[1] && p < best[0])) best = [p, n];
    if (!best) continue;
    const idx = prev.index.get(best[0]);
    const size = prev.members.get(best[0])?.size ?? 0;
    if (idx == null || idx >= OTHER || used.has(idx) || best[1] * 2 < size) continue;
    out.set(cat, idx);
    used.add(idx);
    claimed.add(best[0]);
  }
  let next = 0;
  for (const cat of order) {
    if (out.has(cat)) continue;
    while (next < OTHER && used.has(next)) next++;
    if (next < OTHER) {
      out.set(cat, next);
      used.add(next);
    } else out.set(cat, OTHER);
  }
  return out;
}

/** What a colour lens's run leaves for the next run's {@link stableIndex}. */
export function colourMemory(r: LensResult): ColourMemory {
  const members = new Map<string, Set<string>>();
  const index = new Map<string, number>();
  for (const [id, v] of r.values) {
    if (typeof v !== 'string') continue;
    let m = members.get(v);
    if (!m) members.set(v, (m = new Set()));
    m.add(id);
    const c = r.codes.get(id);
    if (c != null) index.set(v, c);
  }
  return { members, index };
}

/** A colour lens over categories (§2.1): twelve qualitative colours by size, then "other". */
export function colourResult(
  c: Common & {
    values: Map<string, string | null>;
    label?: (v: string) => string;
    /** The palette index of each category, such as {@link stableIndex} gives. */
    index?: Map<string, number>;
    /** A legend entry's text, such as "community 12, 41 drawn of 3,980". */
    entry?: (v: string, drawn: number) => string;
    /** What the categories are called in the summary, "categories" by default. */
    noun?: [string, string];
  },
): LensResult {
  const label = c.label ?? ((v: string) => v);
  const ordered = categoryOrder(c.values, label);
  const index = c.index ?? ordered.index;
  // in palette order, so that a kept colour keeps its place in the legend too
  const order = c.index
    ? [...ordered.order].sort(
        (a, b) =>
          (index.get(a) ?? OTHER) - (index.get(b) ?? OTHER) ||
          ordered.index.get(a)! - ordered.index.get(b)!,
      )
    : ordered.order;
  const entry = c.entry ?? ((v: string) => label(v));
  const [one, many] = c.noun ?? ['category', 'categories'];
  const codes = new Map<string, number>();
  const text = new Map<string, string>();
  const byCat = new Map<string, string[]>();
  const none: string[] = [];
  for (const [id, v] of c.values) {
    if (v == null) {
      none.push(id);
      text.set(id, 'no value');
      continue;
    }
    codes.set(id, index.get(v)!);
    text.set(id, label(v));
    const list = byCat.get(v);
    if (list) list.push(id);
    else byCat.set(v, [id]);
  }
  const coloured = order.filter((v) => (index.get(v) ?? OTHER) < OTHER);
  const legend: LegendEntry[] = coloured.map((v) => ({
    key: `cat-${v}`,
    text: entry(v, byCat.get(v)!.length),
    count: byCat.get(v)!.length,
    ids: byCat.get(v)!,
    swatch: index.get(v)!,
  }));
  const rest = order.filter((v) => (index.get(v) ?? OTHER) >= OTHER);
  if (rest.length) {
    const ids = rest.flatMap((v) => byCat.get(v)!);
    legend.push({
      key: 'other',
      text: `other, ${rest.length} ${rest.length === 1 ? one : many}`,
      count: ids.length,
      ids,
      swatch: OTHER,
    });
  }
  if (none.length) legend.push({ key: 'none', text: 'no value', count: none.length, ids: none });
  return {
    channel: 'colour',
    lens: c.lens,
    name: c.name,
    title: `colour is ${c.name}`,
    values: new Map<string, LensValue>(c.values),
    codes,
    text,
    legend,
    summary: `${order.length} ${order.length === 1 ? one : many}`,
    computedAt: c.computedAt,
  };
}

/** A badge lens: a glyph with a count in the corner of each node with a count above zero. */
export function badgeResult(c: Common & { values: Map<string, number> }): LensResult {
  const codes = new Map<string, number>();
  const text = new Map<string, string>();
  const ids: string[] = [];
  for (const [id, n] of c.values) {
    if (n <= 0) continue;
    codes.set(id, n);
    text.set(id, String(n));
    ids.push(id);
  }
  return {
    channel: 'badge',
    lens: c.lens,
    name: c.name,
    title: `badges count ${c.name}`,
    values: new Map<string, LensValue>(c.values),
    codes,
    text,
    legend: ids.length
      ? [{ key: 'badge', text: `nodes with ${c.name}`, count: ids.length, ids }]
      : [],
    computedAt: c.computedAt,
  };
}

export type EdgeEnd = { id: string; source: string; target: string };

/**
 * The inferred-edge lens (§2.2): dashed edges with an `i` marker for the drawn edges the
 * reasoner added. `inferred` holds their ids and `unchecked` the ids of edges a read cannot
 * name, such as those with a blank node, which are drawn as they are.
 */
export function inferredResult(
  c: Common & {
    edges: readonly EdgeEnd[];
    inferred: ReadonlySet<string>;
    unchecked: ReadonlySet<string>;
  },
): LensResult {
  const values = new Map<string, LensValue>();
  const codes = new Map<string, number>();
  const text = new Map<string, string>();
  const ends = new Set<string>();
  let n = 0;
  for (const e of c.edges) {
    if (c.unchecked.has(e.id)) {
      values.set(e.id, null);
      continue;
    }
    const inf = c.inferred.has(e.id);
    values.set(e.id, inf ? 'inferred' : 'asserted');
    if (inf) {
      n++;
      codes.set(e.id, 1);
      text.set(e.id, 'by the reasoner');
      ends.add(e.source);
      ends.add(e.target);
    }
  }
  const legend: LegendEntry[] = [
    { key: 'inferred', text: 'inferred edges', count: n, ids: [...ends] },
  ];
  if (c.unchecked.size)
    legend.push({
      key: 'unchecked',
      text: 'edges with a blank node or a triple term were not checked',
      count: c.unchecked.size,
      ids: [],
    });
  return {
    channel: 'edge',
    lens: c.lens,
    name: c.name,
    title: 'dashed edges were inferred by the reasoner',
    values,
    codes,
    text,
    legend,
    summary: `${n} of ${c.edges.length} ${c.edges.length === 1 ? 'edge' : 'edges'} inferred`,
    computedAt: c.computedAt,
  };
}

export type Results = Partial<Record<Channel, LensResult | null>>;

/** What a node looks like under the active lenses. */
export type NodeLook = {
  /** A size step, or `NO_VALUE`. */
  size?: number;
  /** A palette index, `OTHER` for "other". */
  colour?: number;
  /** The badge's count. */
  badge?: number;
};

/** The encoding table both views draw from: what each element looks like. */
export type Encoding = {
  nodes: Map<string, NodeLook>;
  /** Edge ids drawn dashed with an `i` marker. */
  inferred: Set<string>;
  size: boolean;
  colour: boolean;
  badge: boolean;
  /** The edge lens is on, so dashes mean inferred edges only. */
  edge: boolean;
};

export const NO_ENCODING: Encoding = {
  nodes: new Map(),
  inferred: new Set(),
  size: false,
  colour: false,
  badge: false,
  edge: false,
};

/** The encoding of a set of lens results. */
export function encode(results: Results): Encoding {
  const nodes = new Map<string, NodeLook>();
  const look = (id: string) => {
    let l = nodes.get(id);
    if (!l) nodes.set(id, (l = {}));
    return l;
  };
  const size = results.size;
  const colour = results.colour;
  const badge = results.badge;
  const edge = results.edge;
  if (size) for (const [id, s] of size.codes) look(id).size = s;
  if (colour) for (const [id, c] of colour.codes) look(id).colour = c;
  if (badge) for (const [id, b] of badge.codes) look(id).badge = b;
  return {
    nodes,
    inferred: new Set(edge ? edge.codes.keys() : []),
    size: !!size,
    colour: !!colour,
    badge: !!badge,
    edge: !!edge,
  };
}

/**
 * The nodes of the legend entry `key` (`channel:entry`), or null when no active lens has
 * that entry any more, so that a selection does not outlive its entry.
 */
export function entryIds(results: Results, key: string | null): string[] | null {
  if (!key) return null;
  const i = key.indexOf(':');
  const r = results[key.slice(0, i) as Channel];
  const e = r?.legend.find((x) => x.key === key.slice(i + 1));
  return e ? e.ids : null;
}

/** A node's or an edge's lens values as text, such as "degree 12, class Person". */
export function describe(id: string, results: Results): string {
  return CHANNELS.flatMap((ch) => {
    const r = results[ch];
    const t = r?.text.get(id);
    return r && t ? [`${r.name} ${t}`] : [];
  }).join(', ');
}

/**
 * What the live region says when a lens has its result (§2.5), such as "Size by degree in
 * the drawn graph on 214 nodes, top node ex:acme with 81".
 */
export function announcement(r: LensResult, name: string, label: (id: string) => string): string {
  if (r.channel === 'size') {
    let top: [string, number] | null = null;
    for (const [id, v] of r.values)
      if (typeof v === 'number' && (!top || v > top[1])) top = [id, v];
    const on = `${r.values.size} ${r.values.size === 1 ? 'node' : 'nodes'}`;
    return top
      ? `Size by ${name} on ${on}, top node ${label(top[0])} with ${fmt(top[1])}`
      : `Size by ${name} on ${on}, no node has a value`;
  }
  if (r.channel === 'colour')
    return `Colour by ${name} on ${r.values.size} ${r.values.size === 1 ? 'node' : 'nodes'}, ${r.summary}`;
  if (r.channel === 'badge') {
    const n = r.codes.size;
    return `${n} ${n === 1 ? 'node has' : 'nodes have'} ${name}`;
  }
  return `${r.summary ?? ''}`.replace(/^./, (c) => c.toUpperCase());
}

/** The SVG of a badge, as a data URI for Cytoscape's background image. */
export function badgeSvg(n: number, fill: string, ink: string): string {
  const t = n > 99 ? '99+' : String(n);
  const w = 10 + t.length * 6;
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="14" viewBox="0 0 ${w} 14">` +
    `<rect x="0.5" y="0.5" width="${w - 1}" height="13" rx="6.5" fill="${fill}" stroke="${ink}" stroke-opacity="0.35"/>` +
    `<text x="${w / 2}" y="10.5" font-family="sans-serif" font-size="10" font-weight="700" text-anchor="middle" fill="${ink}">i${t}</text></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}
