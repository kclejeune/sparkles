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

/**
 * Where a node the paths add is placed in a view that is already laid out: between its
 * neighbours on the paths that have places, else at `fallback`.
 */
export function placeBetween(
  id: string,
  edges: readonly Link[],
  at: (id: string) => { x: number; y: number } | undefined,
  fallback: { x: number; y: number },
): { x: number; y: number } {
  const near: { x: number; y: number }[] = [];
  for (const e of edges) {
    if (!e.paths?.length) continue;
    const other = e.source === id ? e.target : e.target === id ? e.source : null;
    const p = other != null ? at(other) : undefined;
    if (p) near.push(p);
  }
  if (!near.length) return fallback;
  const x = near.reduce((a, p) => a + p.x, 0) / near.length;
  const y = near.reduce((a, p) => a + p.y, 0) / near.length;
  // a small offset, so that a node between two others does not sit on their edge
  return { x: x + 3, y: y + 3 };
}
