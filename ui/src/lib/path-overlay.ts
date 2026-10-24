// The path finder's overlay on a graph that a page has already drawn (U09 Phase 3): the
// query page's result graph, in the canvas view or the WebGL view. The drawn graph names
// its nodes and edges its own way, so path edges are matched by their triple, and the
// nodes on a path are the ends of its matched edges. That keeps the per-triple literal
// nodes of a result graph apart: two triples that end in the same literal end in two
// nodes, and each carries only the paths of its own triple.
import type { GEdge, GNode } from '$components/GraphView.svelte';
import type { Term } from './api';
import { edgeId, type OverlayEdge, type OverlayNode } from './paths';
import { RDF_TYPE, termKey } from './rdf';

export type PathOverlay = { nodes: OverlayNode[]; edges: OverlayEdge[] };

/** The key of an edge's triple, as `paths.ts` names path edges. */
function tripleKey(e: GEdge): string | null {
  if (!e.triple) return null;
  const [s, p, o] = e.triple;
  return p.type === 'uri' ? edgeId(s, p.value, o) : null;
}

export type MergedPaths = {
  nodes: GNode[];
  edges: GEdge[];
  /** The ids, in the merged graph, of the nodes and edges of each path. */
  members: Map<number, { nodes: Set<string>; edges: Set<string> }>;
  /** Nodes and edges the drawn graph did not have. */
  added: { nodes: number; edges: number };
};

const sorted = (s: Set<number>) => [...s].sort((a, b) => a - b);

/**
 * The drawn graph with the paths on it: drawn elements on a path carry its numbers and
 * the order of the animation, and the paths' other elements are added. `label` names an
 * added node, and `edgeLabel` an added edge's predicate.
 */
export function mergePaths(
  nodes: readonly GNode[],
  edges: readonly GEdge[],
  o: PathOverlay,
  label: (t: Term) => string,
  edgeLabel: (p: string) => string,
): MergedPaths {
  const byTriple = new Map<string, GEdge>();
  for (const e of edges) {
    const k = tripleKey(e);
    if (k && !byTriple.has(k)) byTriple.set(k, e);
  }
  // an IRI's or blank node's id in the drawn graph, through the ends of matched edges
  const idOf = new Map<string, string>();
  for (const e of o.edges) {
    const d = byTriple.get(e.id);
    if (!d) continue;
    if (e.s.type !== 'literal') idOf.set(termKey(e.s), d.source);
    if (e.o.type !== 'literal') idOf.set(termKey(e.o), d.target);
  }
  const order = new Map(o.nodes.map((n) => [n.key, n.order]));
  /** Where an end of a path edge is drawn: a literal gets a node per triple. */
  const endOf = (t: Term, drawn: string | undefined, eid: string, side: 's' | 'o') => {
    if (drawn) return drawn;
    const k = termKey(t);
    return t.type === 'literal' ? `path:${eid}|${side}` : (idOf.get(k) ?? k);
  };
  // the path numbers of each node on a path, and the term and order of each
  const onNode = new Map<string, { term: Term; paths: Set<number>; order: number }>();
  const mark = (id: string, t: Term, paths: number[]) => {
    let n = onNode.get(id);
    if (!n) onNode.set(id, (n = { term: t, paths: new Set(), order: order.get(termKey(t)) ?? 0 }));
    for (const p of paths) n.paths.add(p);
  };
  const onEdges = new Map<string, OverlayEdge>();
  const addEdges: { e: OverlayEdge; source: string; target: string }[] = [];
  for (const e of o.edges) {
    const d = byTriple.get(e.id);
    const source = endOf(e.s, d?.source, e.id, 's');
    const target = endOf(e.o, d?.target, e.id, 'o');
    mark(source, e.s, e.paths);
    mark(target, e.o, e.paths);
    if (d) onEdges.set(d.id, e);
    else addEdges.push({ e, source, target });
  }

  const members = new Map<number, { nodes: Set<string>; edges: Set<string> }>();
  const member = (n: number) => {
    let m = members.get(n);
    if (!m) members.set(n, (m = { nodes: new Set(), edges: new Set() }));
    return m;
  };
  const have = new Set(nodes.map((n) => n.id));
  const outNodes: GNode[] = nodes.map((n) => {
    const p = onNode.get(n.id);
    if (!p) return n;
    for (const k of p.paths) member(k).nodes.add(n.id);
    return { ...n, paths: sorted(p.paths) };
  });
  let addedNodes = 0;
  for (const [nid, p] of onNode) {
    if (have.has(nid)) continue;
    addedNodes++;
    for (const k of p.paths) member(k).nodes.add(nid);
    outNodes.push({
      id: nid,
      label: label(p.term),
      kind: p.term.type === 'uri' ? 'iri' : p.term.type === 'literal' ? 'literal' : 'bnode',
      title: p.term.type === 'uri' ? p.term.value : undefined,
      term: p.term,
      paths: sorted(p.paths),
      order: p.order,
    });
  }
  const outEdges: GEdge[] = edges.map((e) => {
    const p = onEdges.get(e.id);
    if (!p) return e;
    for (const k of p.paths) member(k).edges.add(e.id);
    return { ...e, paths: p.paths };
  });
  for (const { e, source, target } of addEdges) {
    const eid = `path:${e.id}`;
    for (const k of e.paths) member(k).edges.add(eid);
    outEdges.push({
      id: eid,
      source,
      target,
      label: edgeLabel(e.p),
      title: e.p,
      isType: e.p === RDF_TYPE,
      triple: [e.s, { type: 'uri', value: e.p }, e.o],
      paths: e.paths,
      order: e.order,
    });
  }
  return {
    nodes: outNodes,
    edges: outEdges,
    members,
    added: { nodes: addedNodes, edges: addEdges.length },
  };
}

type Link = { source: string; target: string; paths?: number[] };
type Point = { x: number; y: number };
type Rings = { count: Map<string, number>; dir: Map<string, Point> };

/**
 * Where the nodes a path adds go in a view that is already laid out. The added nodes that
 * path edges join form groups, and each group is placed by the drawn nodes it touches:
 *
 * - With two or more, each node moves to the mean of its path neighbours a few dozen times,
 *   so a chain between two drawn nodes spreads evenly along the way between them.
 * - With one, the group fans out from it, one `gap` per edge away from it, towards where
 *   the group's fallbacks lie.
 * - With none, the nodes keep their fallbacks, which are apart.
 *
 * Only the edges on a path are read, once, so a large graph without paths costs one pass
 * over its edges, and every new node keeps its fallback.
 */
export function placeAdded(
  added: readonly { id: string; fallback: Point }[],
  edges: readonly Link[],
  at: (id: string) => Point | undefined,
  gap = 20,
): Map<string, Point> {
  const fallback = new Map(added.map((n) => [n.id, n.fallback]));
  const near = new Map<string, string[]>();
  const link = (a: string, b: string) => {
    let l = near.get(a);
    if (!l) near.set(a, (l = []));
    l.push(b);
  };
  for (const e of edges) {
    if (!e.paths?.length || e.source === e.target) continue;
    if (fallback.has(e.source)) link(e.source, e.target);
    if (fallback.has(e.target)) link(e.target, e.source);
  }
  const out = new Map<string, Point>();
  const seen = new Set<string>();
  // the groups hanging from one drawn node share its direction and its rings, so that none
  // of their nodes takes another's place
  const rings: Rings = { count: new Map(), dir: new Map() };
  for (const n of added) {
    if (seen.has(n.id)) continue;
    // the group of added nodes that path edges join, and the drawn nodes it touches
    const group: string[] = [];
    const anchors = new Map<string, Point>();
    const todo = [n.id];
    seen.add(n.id);
    while (todo.length) {
      const id = todo.pop()!;
      group.push(id);
      for (const k of near.get(id) ?? []) {
        if (fallback.has(k)) {
          if (!seen.has(k)) {
            seen.add(k);
            todo.push(k);
          }
        } else {
          const p = at(k);
          if (p) anchors.set(k, p);
        }
      }
    }
    if (anchors.size >= 2) between(group, near, fallback, at, out);
    else if (anchors.size === 1) {
      const [[anchor, p]] = [...anchors];
      fanOut(group, anchor, p, near, fallback, gap, rings, out);
    } else for (const id of group) out.set(id, fallback.get(id)!);
  }
  return out;
}

/** A group between two or more drawn nodes, by repeated means. */
function between(
  group: string[],
  near: Map<string, string[]>,
  fallback: Map<string, Point>,
  at: (id: string) => Point | undefined,
  out: Map<string, Point>,
) {
  for (const id of group) {
    const fixed = (near.get(id) ?? []).map(at).filter((p): p is Point => !!p);
    out.set(id, fixed.length ? mean(fixed) : fallback.get(id)!);
  }
  const where = (k: string) => (fallback.has(k) ? out.get(k) : at(k));
  for (let round = 0; round < 60; round++)
    for (const id of group) {
      const ps = (near.get(id) ?? []).map(where).filter((p): p is Point => !!p);
      if (ps.length) out.set(id, mean(ps));
    }
  // a small offset, so that a node between two others does not sit on their edge
  for (const id of group) {
    const p = out.get(id)!;
    out.set(id, { x: p.x + 3, y: p.y + 3 });
  }
}

/** A group hanging from one drawn node: rings of `gap` per edge, side by side within one. */
function fanOut(
  group: string[],
  anchor: string,
  from: Point,
  near: Map<string, string[]>,
  fallback: Map<string, Point>,
  gap: number,
  rings: Rings,
  out: Map<string, Point>,
) {
  // the first group to hang from the anchor sets the direction, towards its fallbacks
  let d = rings.dir.get(anchor);
  if (!d) {
    const c = mean(group.map((id) => fallback.get(id)!));
    const len = Math.hypot(c.x - from.x, c.y - from.y);
    d = len > 1e-9 ? { x: (c.x - from.x) / len, y: (c.y - from.y) / len } : { x: 1, y: 0 };
    rings.dir.set(anchor, d);
  }
  const { x: dx, y: dy } = d;
  // hops from the anchor, breadth first
  const hops = new Map<string, number>();
  let ring = group.filter((id) => near.get(id)?.includes(anchor));
  for (const id of ring) hops.set(id, 1);
  for (let h = 2; ring.length; h++) {
    const next: string[] = [];
    for (const id of ring)
      for (const k of near.get(id) ?? [])
        if (fallback.has(k) && !hops.has(k)) {
          hops.set(k, h);
          next.push(k);
        }
    ring = next;
  }
  for (const id of group) {
    const h = hops.get(id) ?? 1;
    const k = rings.count.get(`${anchor}|${h}`) ?? 0;
    rings.count.set(`${anchor}|${h}`, k + 1);
    // the k-th node of a ring sits beside the others, alternately on either side
    const side = k === 0 ? 0 : (k % 2 ? 1 : -1) * Math.ceil(k / 2) * gap;
    out.set(id, { x: from.x + dx * gap * h - dy * side, y: from.y + dy * gap * h + dx * side });
  }
}

function mean(ps: Point[]): Point {
  return {
    x: ps.reduce((a, p) => a + p.x, 0) / ps.length,
    y: ps.reduce((a, p) => a + p.y, 0) / ps.length,
  };
}
