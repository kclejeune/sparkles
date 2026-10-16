// The Explore graph over time (docs/specs/U01-time-slider.md, "The Explore graph"): what a
// comparison between two commits adds to the drawn neighbourhood, and the changes of the
// schema's counts. Nothing here touches the DOM or the network.
import type { DiffQuad, Term } from './api';
import { parseCompact } from './compact';
import type { Schema } from './explore';
import { termKey, toSparql } from './rdf';
import type { Mark } from './rowdiff';

/** The most removed elements a comparison draws as ghosts. */
export const MAX_GHOSTS = 200;

export type DrawnEdge = { id: string; source: string; target: string };

/** The subject, predicate and object of an edge. */
export type EdgeTriple = [Term, Term, Term];

export type DiffPlan = {
  /** Drawn edges that the later state added. */
  addedEdges: Set<string>;
  /** The triple of each added edge, to check it against the earlier state. */
  addedTriples: Map<string, EdgeTriple>;
  /** Drawn nodes whose every edge was added (the focus is never one of them). */
  addedNodes: Set<string>;
  /** Edges only the earlier state had, drawn as dashed ghosts. */
  ghostEdges: {
    id: string;
    source: string;
    target: string;
    predicate: string;
    lit: boolean;
    triple: EdgeTriple;
  }[];
  /** The ends of those edges that are not drawn. */
  ghostNodes: { id: string; term: Term }[];
  /** More removed edges than are drawn. */
  truncated: boolean;
};

/** An N-Triples term of a diff quad. */
function termOf(nt: string): Term {
  return parseCompact(nt, {});
}

/** The IRI of a predicate the diff gives as `<iri>`. */
function predicateOf(nt: string): string {
  const t = termOf(nt);
  return t.type === 'uri' ? t.value : nt;
}

/** The id the Explore graph gives the edge from `s` over `p` to `o`, and the node of a literal. */
function ids(s: string, p: string, o: Term) {
  const lit = o.type === 'literal';
  const oid = lit ? `lit:${s}|${p}|${termKey(o)}` : termKey(o);
  return { oid, lit, edge: `${s}|${p}|${oid}` };
}

/**
 * What the changes between two commits mean for the drawn graph. An added quad marks its
 * edge when the edge is drawn. A removed quad becomes a ghost edge when one of its ends is
 * drawn, with a ghost node for the other end when that is not drawn.
 */
export function planDiff(
  quads: readonly DiffQuad[],
  nodeIds: ReadonlySet<string>,
  edges: readonly DrawnEdge[],
  focus: string | null,
  max = MAX_GHOSTS,
): DiffPlan {
  const drawn = new Set(edges.map((e) => e.id));
  const plan: DiffPlan = {
    addedEdges: new Set(),
    addedTriples: new Map(),
    addedNodes: new Set(),
    ghostEdges: [],
    ghostNodes: [],
    truncated: false,
  };
  const ghostIds = new Set<string>();
  const addGhostNode = (id: string, term: Term) => {
    if (nodeIds.has(id) || ghostIds.has(id)) return;
    ghostIds.add(id);
    plan.ghostNodes.push({ id, term });
  };
  for (const q of quads) {
    const s = termOf(q.subject);
    const o = termOf(q.object);
    const p = predicateOf(q.predicate);
    const sid = termKey(s);
    const { oid, lit, edge } = ids(sid, p, o);
    if (q.op === '+') {
      if (drawn.has(edge)) {
        plan.addedEdges.add(edge);
        plan.addedTriples.set(edge, [s, { type: 'uri', value: p }, o]);
      }
      continue;
    }
    // a removed quad is drawn only next to something that is drawn
    if (!nodeIds.has(sid) && !(!lit && nodeIds.has(oid))) continue;
    if (drawn.has(edge) || plan.ghostEdges.some((g) => g.id === edge)) continue;
    if (plan.ghostEdges.length >= max) {
      plan.truncated = true;
      continue;
    }
    addGhostNode(sid, s);
    addGhostNode(oid, o);
    plan.ghostEdges.push({
      id: edge,
      source: sid,
      target: oid,
      predicate: p,
      lit,
      triple: [s, { type: 'uri', value: p }, o],
    });
  }
  plan.addedNodes = addedNodesOf(edges, plan.addedEdges, focus);
  return plan;
}

/** A node is new when everything drawn at it is new. */
function addedNodesOf(
  edges: readonly DrawnEdge[],
  addedEdges: ReadonlySet<string>,
  focus: string | null,
): Set<string> {
  const incident = new Map<string, number>();
  const incidentAdded = new Map<string, number>();
  for (const e of edges) {
    for (const n of [e.source, e.target]) {
      incident.set(n, (incident.get(n) ?? 0) + 1);
      if (addedEdges.has(e.id)) incidentAdded.set(n, (incidentAdded.get(n) ?? 0) + 1);
    }
  }
  const out = new Set<string>();
  for (const [n, count] of incident) if (n !== focus && count === incidentAdded.get(n)) out.add(n);
  return out;
}

/** The id the drawn graph gives the edge of a triple. */
export function edgeIdOf([s, p, o]: EdgeTriple): string {
  return ids(termKey(s), p.type === 'uri' ? p.value : termKey(p), o).edge;
}

/** Most triples one candidate query checks. */
export const CANDIDATES_PER_QUERY = 200;

/**
 * A query for which of `triples` hold in a state, as the Explore graph reads it (the
 * dataset's default graph view, with its union or inference settings), whatever graph a
 * diff quad was in. Blank nodes and triple terms cannot stand in VALUES, so those triples
 * are left out and keep their marks. Empty when none is left.
 */
export function candidateQuery(triples: readonly EdgeTriple[]): string {
  const rows = triples
    .filter((t) => t.every((x) => x.type === 'uri' || x.type === 'literal'))
    .map(([s, p, o]) => `(${toSparql(s)} ${toSparql(p)} ${toSparql(o)})`);
  if (!rows.length) return '';
  return `SELECT ?s ?p ?o WHERE { VALUES (?s ?p ?o) { ${rows.join(' ')} } ?s ?p ?o }`;
}

/**
 * The plan once the candidate queries have answered. A diff lists changes of every graph, and
 * the drawn graph shows one view of them: an edge marked added that the earlier state already
 * had (`presentBefore`, ids of edges) was added in another graph only, and a ghost edge that
 * the later state still has (`presentAfter`) was removed from another graph only. Their marks
 * go, and a node that had only such edges is no longer new.
 */
export function confirmPlan(
  plan: DiffPlan,
  edges: readonly DrawnEdge[],
  focus: string | null,
  presentBefore: ReadonlySet<string>,
  presentAfter: ReadonlySet<string>,
): DiffPlan {
  const addedEdges = new Set([...plan.addedEdges].filter((e) => !presentBefore.has(e)));
  const addedTriples = new Map([...plan.addedTriples].filter(([e]) => addedEdges.has(e)));
  const ghostEdges = plan.ghostEdges.filter((g) => !presentAfter.has(g.id));
  const ends = new Set(ghostEdges.flatMap((g) => [g.source, g.target]));
  return {
    ...plan,
    addedEdges,
    addedTriples,
    addedNodes: addedNodesOf(edges, addedEdges, focus),
    ghostEdges,
    ghostNodes: plan.ghostNodes.filter((n) => ends.has(n.id)),
  };
}

// --- the schema over time ------------------------------------------------------------------

export type CountChange = {
  /** Instances of a class, or triples of a property, now minus then. */
  delta: number;
  mark: Mark;
};

export type SchemaChanges = {
  classes: Map<string, CountChange>;
  properties: Map<string, CountChange>;
  /** Classes and properties the earlier state had and the later one lacks. */
  removedClasses: { iri: string; label?: string; instances: number }[];
  removedProperties: { iri: string; label?: string; triples: number }[];
};

/** `+120`, `−4`, or `0`. */
export function deltaText(n: number): string {
  if (n > 0) return `+${n.toLocaleString('en-US')}`;
  if (n < 0) return `−${Math.abs(n).toLocaleString('en-US')}`;
  return '0';
}

/**
 * The changes of a schema's counts from `from` to `to`: each class and property whose count
 * differs, those only `to` has as added, and those only `from` has as removed.
 */
export function schemaChanges(from: Schema, to: Schema): SchemaChanges {
  const out: SchemaChanges = {
    classes: new Map(),
    properties: new Map(),
    removedClasses: [],
    removedProperties: [],
  };
  for (const [iri, c] of to.classes) {
    const before = from.classes.get(iri);
    if (!before) out.classes.set(iri, { delta: c.instances, mark: 'added' });
    else if (before.instances !== c.instances)
      out.classes.set(iri, { delta: c.instances - before.instances, mark: 'kept' });
  }
  for (const [iri, c] of from.classes)
    if (!to.classes.has(iri))
      out.removedClasses.push({ iri, label: c.label, instances: c.instances });
  const was = new Map(from.properties.map((p) => [p.iri, p]));
  const now = new Set(to.properties.map((p) => p.iri));
  for (const p of to.properties) {
    const before = was.get(p.iri);
    const triples = p.observed.triples;
    if (!before) out.properties.set(p.iri, { delta: triples, mark: 'added' });
    else if (before.observed.triples !== triples)
      out.properties.set(p.iri, { delta: triples - before.observed.triples, mark: 'kept' });
  }
  for (const p of from.properties)
    if (!now.has(p.iri))
      out.removedProperties.push({ iri: p.iri, label: p.label, triples: p.observed.triples });
  out.removedClasses.sort((a, b) => a.iri.localeCompare(b.iri));
  out.removedProperties.sort((a, b) => a.iri.localeCompare(b.iri));
  return out;
}

/** "3 classes and 2 properties changed", for the live region. */
export function schemaAnnouncement(c: SchemaChanges): string {
  const classes = c.classes.size + c.removedClasses.length;
  const props = c.properties.size + c.removedProperties.length;
  if (!classes && !props) return 'The schema did not change.';
  const parts = [
    classes ? `${classes} ${classes === 1 ? 'class' : 'classes'}` : '',
    props ? `${props} ${props === 1 ? 'property' : 'properties'}` : '',
  ].filter(Boolean);
  return `${parts.join(' and ')} changed.`;
}
