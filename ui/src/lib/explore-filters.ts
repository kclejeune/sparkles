// What Explore draws of the graph it holds (ROADMAP item 725). The explored graph can hold
// far more nodes and edges than a canvas can draw, so the filters here choose what is drawn
// and never what is loaded: a predicate facet that hides each edge type, a class facet for
// nodes, a minimum degree, hiding leaves, the groups of a colour lens such as the
// communities, and a budget of visible nodes. Every function is pure, over the minimal shape
// of a node and an edge, so that the page and the tests share them.

/** The nodes Explore draws at most, a budget of visible nodes, not of held ones. */
export const VISIBLE_BUDGET = 5000;
/** The budgets the filter panel offers. */
export const BUDGETS: readonly number[] = [1000, 2000, 5000, 10000, 20000];
/** The nodes the explored graph holds at most, past which expanding asks to clear some. */
export const HELD_LIMIT = 100_000;
/** The visible nodes past which the page draws the graph with WebGL. */
export const WEBGL_FROM = 1000;
/** Once on WebGL, the visible nodes under which the page draws with the canvas again. */
export const CANVAS_BELOW = 700;

/** The key the class facet gives nodes that have no `rdf:type` edge in the held graph. */
export const NO_CLASS = '';

export type FNode = {
  id: string;
  /** A literal, blank node, triple or resource: only resources have classes. */
  kind: string;
  /** For a group node: the predicate it stands for. */
  via?: string;
};
export type FEdge = {
  id: string;
  source: string;
  target: string;
  /** The predicate IRI. */
  title?: string;
  isType?: boolean;
  /** A path of the path finder's overlay runs along the edge, which keeps it drawn. */
  paths?: readonly unknown[];
};

const pred = (e: FEdge) => e.title ?? '';

export type Filters = {
  /** Predicate IRIs whose edges are not drawn. */
  predicates: ReadonlySet<string>;
  /** Class ids whose members are not drawn, with `NO_CLASS` for nodes without one. */
  classes: ReadonlySet<string>;
  /** Draw only nodes with at least this many drawn edges. */
  minDegree: number;
  /** Do not draw nodes with one drawn edge. */
  hideLeaves: boolean;
  /** Nodes of the groups the person hid, such as the communities of a colour lens. */
  hiddenIds: ReadonlySet<string>;
  /** The most nodes to draw. */
  budget: number;
  /**
   * Do not draw the `rdf:type` edges. They still give the nodes their classes, and they no
   * longer count towards a node's degree.
   */
  hideTypes?: boolean;
};

export const NO_FILTERS: Filters = {
  predicates: new Set(),
  classes: new Set(),
  minDegree: 0,
  hideLeaves: false,
  hiddenIds: new Set(),
  budget: VISIBLE_BUDGET,
};

/** Whether the filters change anything beyond the budget. */
export function filtering(f: Filters): boolean {
  return !!(
    f.predicates.size ||
    f.classes.size ||
    f.minDegree > 0 ||
    f.hideLeaves ||
    f.hiddenIds.size
  );
}

export type FacetEntry = { key: string; count: number };

/** The edge types of the held edges, most frequent first, ties by predicate. */
export function predicateFacet(edges: readonly FEdge[]): FacetEntry[] {
  const n = new Map<string, number>();
  for (const e of edges) n.set(pred(e), (n.get(pred(e)) ?? 0) + 1);
  return sorted(n);
}

/**
 * The classes of each node: the targets of its `rdf:type` edges. A node with none is not in
 * the map. `kindOf` is the node's kind, since only resources are typed.
 */
export function classesOf(edges: readonly FEdge[]): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const e of edges) {
    if (!e.isType) continue;
    const l = out.get(e.source);
    if (l) l.push(e.target);
    else out.set(e.source, [e.target]);
  }
  return out;
}

/** The classes of the held nodes, most frequent first, and the untyped resources last. */
export function classFacet(nodes: readonly FNode[], edges: readonly FEdge[]): FacetEntry[] {
  const of = classesOf(edges);
  const n = new Map<string, number>();
  for (const node of nodes) {
    if (node.kind !== 'iri' || node.via !== undefined) continue;
    const cs = of.get(node.id);
    if (!cs) n.set(NO_CLASS, (n.get(NO_CLASS) ?? 0) + 1);
    else for (const c of new Set(cs)) n.set(c, (n.get(c) ?? 0) + 1);
  }
  const untyped = n.get(NO_CLASS);
  n.delete(NO_CLASS);
  const out = sorted(n);
  if (untyped) out.push({ key: NO_CLASS, count: untyped });
  return out;
}

function sorted(n: Map<string, number>): FacetEntry[] {
  return [...n]
    .map(([key, count]) => ({ key, count }))
    .sort((a, b) => b.count - a.count || a.key.localeCompare(b.key));
}

export type Drawn<N extends FNode, E extends FEdge> = {
  nodes: N[];
  edges: E[];
  /** The held nodes the filters and the budget left out. */
  hidden: number;
  /** Of them, the nodes only the budget left out. */
  overBudget: number;
};

/**
 * The nodes and edges to draw. Nodes in `keep`, such as the selection and the focus, are
 * always drawn. The minimum degree counts the edges that survive the predicate and class
 * filters, in one pass, so that hiding leaves does not make new leaves of their neighbours.
 * When more nodes remain than the budget, the most connected ones are drawn.
 */
export function applyFilters<N extends FNode, E extends FEdge>(
  nodes: readonly N[],
  edges: readonly E[],
  f: Filters,
  keep: ReadonlySet<string> = new Set(),
): Drawn<N, E> {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const typed = f.classes.size ? classesOf(edges) : null;
  // 1. nodes by class and by hidden group
  const alive = new Set<string>();
  for (const n of nodes) {
    if (keep.has(n.id)) {
      alive.add(n.id);
      continue;
    }
    if (f.hiddenIds.has(n.id)) continue;
    if (n.via !== undefined && f.predicates.has(n.via)) continue;
    if (typed && n.kind === 'iri' && n.via === undefined) {
      const cs = typed.get(n.id);
      const shown = cs ? cs.some((c) => !f.classes.has(c)) : !f.classes.has(NO_CLASS);
      if (!shown) continue;
    }
    alive.add(n.id);
  }
  // 2. edges by predicate and by their ends
  const live: E[] = [];
  for (const e of edges) {
    if (f.predicates.has(pred(e))) continue;
    if (f.hideTypes && e.isType && !e.paths?.length) continue;
    if (!alive.has(e.source) || !alive.has(e.target)) continue;
    live.push(e);
  }
  // 3. degree, minimum degree and leaves
  const degree = new Map<string, number>();
  for (const e of live) {
    degree.set(e.source, (degree.get(e.source) ?? 0) + 1);
    degree.set(e.target, (degree.get(e.target) ?? 0) + 1);
  }
  const min = Math.max(f.minDegree, f.hideLeaves ? 2 : 0);
  const shown = new Set<string>();
  if (min > 0) {
    for (const id of alive)
      if (keep.has(id) || (degree.get(id) ?? 0) >= min || byId.get(id)?.via !== undefined)
        shown.add(id);
  } else {
    for (const id of alive) shown.add(id);
  }
  // 4. the budget
  let overBudget = 0;
  if (shown.size > f.budget) {
    const order = [...shown].sort(
      (a, b) =>
        Number(keep.has(b)) - Number(keep.has(a)) ||
        (degree.get(b) ?? 0) - (degree.get(a) ?? 0) ||
        (a < b ? -1 : a > b ? 1 : 0),
    );
    for (const id of order.slice(f.budget)) shown.delete(id);
    overBudget = order.length - f.budget;
  }
  const outNodes = nodes.filter((n) => shown.has(n.id));
  const outEdges = live.filter((e) => shown.has(e.source) && shown.has(e.target));
  return { nodes: outNodes, edges: outEdges, hidden: nodes.length - outNodes.length, overBudget };
}

/** Whether the visible node count calls for WebGL, with some hysteresis once it is on. */
export function wantWebgl(visible: number, on: boolean, from = WEBGL_FROM): boolean {
  return on ? visible > Math.min(CANVAS_BELOW, from) : visible > from;
}

/**
 * The edges of each ordered pair of nodes, by the id of the first edge of the pair. A graph
 * of simple edges, such as the WebGL view's, draws one line for the pair, so the page uses
 * this to say which predicates that line stands for. Only pairs of two or more edges are
 * listed.
 */
export function parallelEdges<E extends { id: string; source: string; target: string }>(
  edges: readonly E[],
): Map<string, E[]> {
  const first = new Map<string, string>();
  const groups = new Map<string, E[]>();
  for (const e of edges) {
    if (e.source === e.target) continue;
    const k = `${e.source}\u0001${e.target}`;
    const head = first.get(k);
    if (head === undefined) {
      first.set(k, e.id);
      groups.set(e.id, [e]);
    } else groups.get(head)!.push(e);
  }
  for (const [k, g] of groups) if (g.length < 2) groups.delete(k);
  return groups;
}
