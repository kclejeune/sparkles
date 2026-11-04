// The rows of a tree that draws only the ones in view (ROADMAP item 725): a tree of ids is
// flattened to the rows its open nodes show, and a scroll position picks the window of rows
// to draw. Everything here is plain functions on plain data, so the row math is tested
// without a browser. The component that draws the rows is `OntoTree.svelte`.

/** A node row of the flattened tree. */
export type NodeRow = {
  kind: 'node';
  /** Unique among the rows: a polyhierarchy lists a node once under each parent. */
  key: string;
  id: string;
  depth: number;
  /** The index of the parent's row, or -1 for a root. */
  parent: number;
  /** Whether the node has children, read or still to read. */
  hasKids: boolean;
  open: boolean;
  /** The number of children the tree holds and shows. */
  kids: number;
  /** One-based place among its siblings, for the accessibility tree. */
  pos: number;
  size: number;
};

/** The "Showing N of M · Show more" row that ends a level clipped to one page. */
export type MoreRow = {
  kind: 'more';
  key: string;
  /** The node whose level it ends, or null for the roots. */
  of: string | null;
  depth: number;
  parent: number;
  shown: number;
  total: number;
};

export type TreeRow = NodeRow | MoreRow;

export type FlattenInput = {
  roots: readonly string[];
  children: (id: string) => readonly string[];
  name: (id: string) => string;
  open: (id: string) => boolean;
  /** With a filter: the nodes it keeps, which are all open. */
  keep?: ReadonlySet<string> | null;
  /** Whether a node has children that `children` has not read yet. */
  more?: (id: string) => boolean;
  /** A level that holds fewer children than its node has. */
  clipped?: (parent: string | null) => { shown: number; total: number } | null;
};

const collator = new Intl.Collator(undefined, { sensitivity: 'base' });

/** Ids ordered by their names, as the trees list the children of a level. */
export function sortByName(ids: readonly string[], name: (id: string) => string): string[] {
  if (ids.length < 2) return [...ids];
  const keyed = ids.map((id) => ({ id, name: name(id) }));
  keyed.sort((a, b) => collator.compare(a.name, b.name));
  return keyed.map((k) => k.id);
}

/**
 * The rows a tree shows, depth first: each root, then the children of every open node,
 * with a more-row after a level that holds fewer children than its node has. A node does
 * not appear below itself, and with a filter only the nodes it keeps are listed.
 */
export function flattenTree(input: FlattenInput): TreeRow[] {
  const { children, name, open, keep, more, clipped } = input;
  const rows: TreeRow[] = [];
  const onPath = new Set<string>();

  function level(ids: readonly string[], depth: number, parent: number, of: string | null) {
    const kept = keep ? ids.filter((i) => keep.has(i)) : ids;
    const sorted = sortByName(kept, name);
    const shownIds = sorted.filter((id) => !onPath.has(id));
    const size = shownIds.length;
    const prefix = parent < 0 ? '' : `${rows[parent].key}\u0001`;
    let pos = 0;
    for (const id of shownIds) {
      const kids = children(id);
      const kept = keep ? kids.filter((k) => keep.has(k)) : kids;
      const live = kept.filter((k) => !onPath.has(k));
      const hasKids = live.length > 0 || (!keep && !!more?.(id));
      const isOpen = keep ? true : open(id);
      const at = rows.length;
      rows.push({
        kind: 'node',
        key: `${prefix}${id}`,
        id,
        depth,
        parent,
        hasKids,
        open: isOpen,
        kids: live.length,
        pos: ++pos,
        size,
      });
      if (live.length && isOpen) {
        onPath.add(id);
        level(live, depth + 1, at, id);
        onPath.delete(id);
      }
    }
    const clip = !keep ? (clipped?.(of) ?? null) : null;
    if (clip && clip.shown < clip.total) {
      rows.push({
        kind: 'more',
        key: `${prefix}\u0002more`,
        of,
        depth,
        parent,
        shown: clip.shown,
        total: clip.total,
      });
    }
  }

  level(input.roots, 0, -1, null);
  return rows;
}

/** The index of the row of `id`, or -1. With a polyhierarchy the first one. */
export function indexOfId(rows: readonly TreeRow[], id: string): number {
  return rows.findIndex((r) => r.kind === 'node' && r.id === id);
}

/** The ancestors of the row at `index`, from the root down, as row indexes. */
export function ancestorsOfRow(rows: readonly TreeRow[], index: number): number[] {
  const out: number[] = [];
  for (let p = rows[index]?.parent ?? -1; p >= 0; p = rows[p].parent) out.push(p);
  return out.reverse();
}

/**
 * Where rows sit. Every row is `rowH` tall except the ones in `taller`, which are the
 * selected row's wrapped label and short form: a row index with the height it was measured
 * to have. There are few of them, so a position is a short scan.
 */
export class Metrics {
  /** The indexes of the taller rows, in order, with the pixels each adds. */
  readonly extra: { at: number; add: number }[];

  constructor(
    readonly count: number,
    readonly rowH: number,
    extra: { at: number; add: number }[] = [],
  ) {
    this.extra = extra
      .filter((e) => e.add > 0 && e.at >= 0 && e.at < count)
      .sort((a, b) => a.at - b.at);
  }

  /** The top of row `i`; `count` gives the height of the whole tree. */
  top(i: number): number {
    let y = i * this.rowH;
    for (const e of this.extra) {
      if (e.at >= i) break;
      y += e.add;
    }
    return y;
  }

  get height(): number {
    return this.top(this.count);
  }

  /** The row that holds the vertical position `y`, clamped to the rows. */
  at(y: number): number {
    if (this.count === 0) return 0;
    if (y <= 0) return 0;
    let lo = 0;
    let hi = this.count - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.top(mid) <= y) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }

  /** The rows to draw for a view `height` tall scrolled to `scroll`, with `overscan` extra on each side. */
  window(scroll: number, height: number, overscan = 6): { start: number; end: number } {
    if (this.count === 0) return { start: 0, end: 0 };
    const first = this.at(scroll);
    const last = this.at(scroll + Math.max(0, height));
    return {
      start: Math.max(0, first - overscan),
      end: Math.min(this.count, last + 1 + overscan),
    };
  }
}

/**
 * The scroll position that brings row `i` into a view of `height` with the least movement,
 * under `pinned` pixels at the top that the sticky ancestors cover. Null when it is in view.
 */
export function scrollToRow(
  m: Metrics,
  i: number,
  scroll: number,
  height: number,
  pinned = 0,
): number | null {
  const top = m.top(i);
  const bottom = m.top(i + 1);
  if (top < scroll + pinned) return Math.max(0, top - pinned);
  if (bottom > scroll + height) return Math.max(0, bottom - height);
  return null;
}

/** The most sticky ancestors shown at once. */
export const STICKY_MAX = 6;

/**
 * The ancestors of the first row in view, nearest last, as the sticky rows show them: the
 * nearest `cap` of them, with how many more there are above.
 */
export function stickyPath(
  rows: readonly TreeRow[],
  first: number,
  cap: number,
): { shown: number[]; hidden: number } {
  const path = ancestorsOfRow(rows, first);
  const keep = Math.max(1, Math.min(STICKY_MAX, cap));
  return { shown: path.slice(-keep), hidden: Math.max(0, path.length - keep) };
}

/** How many sticky rows fit: at most 30% of the view, so a short tree keeps most of its rows. */
export function stickyCap(viewHeight: number, rowH: number): number {
  return Math.max(1, Math.min(STICKY_MAX, Math.floor((viewHeight * 0.3) / rowH)));
}

/** A move of the keyboard cursor, in the manner of the WAI-ARIA tree pattern. */
export type KeyMove =
  | { to: number }
  | { toggle: string }
  | { select: string }
  | { more: string | null }
  | null;

/**
 * What a key does when the cursor is on row `at` of `rows` and `page` rows fit a page.
 * Right opens a closed node or enters an open one, Left closes an open node or goes to the
 * parent, and Enter or Space selects, or reads the next page on a more-row.
 */
export function keyMove(rows: readonly TreeRow[], at: number, key: string, page: number): KeyMove {
  if (!rows.length) return null;
  const last = rows.length - 1;
  const i = Math.min(Math.max(at, 0), last);
  const row = rows[i];
  switch (key) {
    case 'ArrowDown':
      return { to: Math.min(last, i + 1) };
    case 'ArrowUp':
      return { to: Math.max(0, i - 1) };
    case 'Home':
      return { to: 0 };
    case 'End':
      return { to: last };
    case 'PageDown':
      return { to: Math.min(last, i + Math.max(1, page)) };
    case 'PageUp':
      return { to: Math.max(0, i - Math.max(1, page)) };
    case 'ArrowRight':
      if (row.kind !== 'node' || !row.hasKids) return null;
      if (!row.open) return { toggle: row.id };
      return rows[i + 1] && rows[i + 1].parent === i ? { to: i + 1 } : null;
    case 'ArrowLeft':
      if (row.kind === 'node' && row.hasKids && row.open) return { toggle: row.id };
      return row.parent >= 0 ? { to: row.parent } : null;
    case 'Enter':
    case ' ':
      return row.kind === 'node' ? { select: row.id } : { more: row.of };
    default:
      return null;
  }
}

/** The trees on screen, so that a link's selection can scroll the tree that holds it. */
export type TreeHandle = { reveal: (id: string) => boolean };
const handles = new Set<TreeHandle>();
export function registerTree(h: TreeHandle): () => void {
  handles.add(h);
  return () => handles.delete(h);
}
/** Scroll the row of `id` into view in whichever tree lists it; false when none does yet. */
export function revealInTrees(id: string): boolean {
  let done = false;
  for (const h of handles) if (h.reveal(id)) done = true;
  return done;
}
