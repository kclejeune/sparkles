// A nested treemap (ROADMAP item 633): the squarified layout of Bruls, Huizing and van Wijk
// for one level, applied down a hierarchy so that each parent's children fill the parent's
// box under a head strip for its title. The Communities tab draws its hierarchy with it, a
// community's area standing for its members. The layout is in pixels of a box the caller
// measured, so the view never scales text.

export type Rect = { x: number; y: number; w: number; h: number };

/** A node of the hierarchy: its weight sets its area. */
export type TreemapNode = { id: string; weight: number; children: TreemapNode[] };

/** A node placed: its box, and whether the view shows its children inside it. */
export type TreemapBox = {
  id: string;
  parent: string | null;
  depth: number;
  rect: Rect;
  /** True when no children are drawn inside, either because it has none or they do not fit. */
  leaf: boolean;
};

const worst = (areas: number[], side: number): number => {
  const sum = areas.reduce((a, b) => a + b, 0);
  const max = Math.max(...areas);
  const min = Math.min(...areas);
  return Math.max((side * side * max) / (sum * sum), (sum * sum) / (side * side * min));
};

/**
 * Splits `rect` into one box per weight, each of an area in proportion to its weight and as
 * near square as the order allows. The boxes come back in the order of the weights. A weight
 * under 1 counts as 1, so every item is drawn.
 */
export function squarify(weights: number[], rect: Rect): Rect[] {
  const out: Rect[] = weights.map(() => ({ x: rect.x, y: rect.y, w: 0, h: 0 }));
  if (!weights.length || rect.w <= 0 || rect.h <= 0) return out;
  const w = weights.map((x) => (x >= 1 ? x : 1));
  const order = w.map((_, i) => i).sort((a, b) => w[b] - w[a] || a - b);
  const scale = (rect.w * rect.h) / w.reduce((a, b) => a + b, 0);
  let { x, y, w: bw, h: bh } = rect;
  let i = 0;
  while (i < order.length) {
    const side = Math.min(bw, bh);
    const row = [order[i]];
    let areas = [w[order[i]] * scale];
    i++;
    // a row grows while its worst aspect ratio improves
    while (i < order.length) {
      const next = [...areas, w[order[i]] * scale];
      if (worst(next, side) > worst(areas, side)) break;
      areas = next;
      row.push(order[i]);
      i++;
    }
    const sum = areas.reduce((a, b) => a + b, 0);
    if (bw >= bh) {
      // a column down the left edge
      const cw = sum / bh;
      let cy = y;
      row.forEach((k, j) => {
        const ch = areas[j] / cw;
        out[k] = { x, y: cy, w: cw, h: ch };
        cy += ch;
      });
      x += cw;
      bw -= cw;
    } else {
      // a row along the top edge
      const rh = sum / bw;
      let cx = x;
      row.forEach((k, j) => {
        const cw = areas[j] / rh;
        out[k] = { x: cx, y, w: cw, h: rh };
        cx += cw;
      });
      y += rh;
      bh -= rh;
    }
  }
  return out;
}

export type TreemapOptions = {
  /** The strip a parent keeps at its top for its title. */
  head?: number;
  /** The space between a parent's edge and its children. */
  pad?: number;
  /** A parent shows its children only when their area is at least this wide and tall. */
  minSide?: number;
};

/**
 * The boxes of a forest laid out in `width` by `height`, parents before their children so
 * that drawing them in order paints a child over its parent.
 */
export function layout(
  forest: TreemapNode[],
  width: number,
  height: number,
  { head = 24, pad = 4, minSide = 36 }: TreemapOptions = {},
): TreemapBox[] {
  const out: TreemapBox[] = [];
  const place = (nodes: TreemapNode[], rect: Rect, depth: number, parent: string | null) => {
    const rects = squarify(
      nodes.map((n) => n.weight),
      rect,
    );
    nodes.forEach((n, i) => {
      const r = rects[i];
      const inner: Rect = {
        x: r.x + pad,
        y: r.y + head,
        w: r.w - 2 * pad,
        h: r.h - head - pad,
      };
      const nest = n.children.length > 0 && inner.w >= minSide && inner.h >= minSide;
      out.push({ id: n.id, parent, depth, rect: r, leaf: !nest });
      if (nest) place(n.children, inner, depth + 1, n.id);
    });
  };
  place(forest, { x: 0, y: 0, w: width, h: height }, 0, null);
  return out;
}
