/**
 * The graph overview (spec U06 §2.3 and §4.3): the class level drawn from the class
 * profiles, and one open path of groups below it. Opening a class or a group replaces its
 * node with the groups or members of the answer of `POST /$/graph/{ds}/groups`, and the
 * rest of the picture stays: the other classes, and the sibling groups of every group on
 * the path. Each answer counts the edges from its own nodes to the classes and to the
 * sibling groups of the path, so the edges of the picture are those of the answers
 * received, kept when both of their ends are drawn.
 *
 * Node keys follow the answers' edge ends: `c:` and a class IRI, `g:` and a group id, and
 * `m:` and a member IRI.
 */
import type { ClassProfile, GroupedDiff, GroupsAnswer } from './api';

export type OvKind = 'class' | 'group' | 'member';

export type OvNode = {
  key: string;
  kind: OvKind;
  label: string;
  /** Instances of a class or a group; 1 for a member. */
  size: number;
  /** The class the node is, or belongs to. */
  cls: string;
  /** The IRI of a class or a member. */
  iri?: string;
  /** The id of a group, for the request that opens it. */
  group?: string;
  /** A group's first members. */
  top?: { iri: string; label?: string }[];
  /** Classes and groups open; members open in the resource view instead. */
  openable: boolean;
  /** Links among the node's own instances, which are not drawn as a loop. */
  internal?: number;
};

export type OvEdge = {
  key: string;
  source: string;
  target: string;
  count: number;
  /** The predicates of the edge, most triples first. `null` for an unnamed one. */
  predicates: { iri: string | null; count: number }[];
};

/** One open node of the path, with the answer that opened it. */
export type OvLevel = { key: string; label: string; answer: GroupsAnswer };

export type OvGraph = { nodes: OvNode[]; edges: OvEdge[] };

/** Classes drawn at most at the class level, the largest first. */
export const MAX_CLASSES = 2000;

/** The class level of a set of profiles: one node per class, one edge per class pair. */
export function classLevel(
  profiles: ClassProfile[],
  opts: { builtins?: boolean; label?: (iri: string) => string } = {},
): OvGraph & { hidden: number } {
  const label = opts.label ?? ((i: string) => i);
  const all = profiles
    .filter((p) => p.instances > 0 && (opts.builtins !== false || !p.builtin))
    .sort((a, b) => b.instances - a.instances || a.class.localeCompare(b.class));
  const shown = all.slice(0, MAX_CLASSES);
  const keys = new Set(shown.map((p) => p.class));
  const nodes: OvNode[] = shown.map((p) => ({
    key: `c:${p.class}`,
    kind: 'class',
    label: label(p.class),
    size: p.instances,
    cls: p.class,
    iri: p.class,
    openable: true,
  }));
  const edges = new Edges();
  const self = new Map<string, number>();
  for (const p of shown) {
    for (const prop of p.properties) {
      for (const oc of prop.objectClasses) {
        if (!keys.has(oc.class)) continue;
        if (oc.class === p.class) {
          self.set(p.class, (self.get(p.class) ?? 0) + oc.triples);
          continue;
        }
        edges.add(`c:${p.class}`, `c:${oc.class}`, prop.predicate, oc.triples);
      }
    }
  }
  for (const n of nodes) {
    const s = self.get(n.cls);
    if (s) n.internal = s;
  }
  return { nodes, edges: edges.list(), hidden: all.length - shown.length };
}

/** Edges merged by their ends. */
class Edges {
  private map = new Map<string, OvEdge>();

  add(source: string, target: string, predicate: string | null, count: number) {
    const key = `${source}\u0001${target}`;
    let e = this.map.get(key);
    if (!e) {
      e = { key, source, target, count: 0, predicates: [] };
      this.map.set(key, e);
    }
    e.count += count;
    const p = e.predicates.find((x) => x.iri === predicate);
    if (p) p.count += count;
    else e.predicates.push({ iri: predicate, count });
  }

  list(): OvEdge[] {
    const out = [...this.map.values()];
    for (const e of out) e.predicates.sort((a, b) => b.count - a.count);
    return out;
  }
}

/** The nodes an answer adds below the node it opened. */
export function answerNodes(a: GroupsAnswer, label: (iri: string) => string): OvNode[] {
  const cls = a.within.class;
  if (a.grouping.kind === 'members') {
    return (a.members ?? []).map((m) => ({
      key: `m:${m.iri}`,
      kind: 'member',
      label: m.label ?? label(m.iri),
      size: 1,
      cls,
      iri: m.iri,
      openable: false,
    }));
  }
  return a.groups.map((g) => ({
    key: `g:${g.id}`,
    kind: 'group',
    label: /^[a-z][a-z0-9+.-]*:\S+$/i.test(g.label) ? label(g.label) : g.label,
    size: g.size,
    cls,
    group: g.id,
    top: g.top,
    openable: g.size > 0,
  }));
}

/**
 * The picture for a class level and a path: the classes but the opened one, the nodes of
 * every answer on the path but those opened further, and the edges between drawn nodes.
 */
export function visible(
  base: OvGraph,
  path: OvLevel[],
  label: (iri: string) => string = (i) => i,
): OvGraph {
  if (!path.length) return base;
  const opened = new Set(path.map((l) => l.key));
  const nodes: OvNode[] = base.nodes.filter((n) => !opened.has(n.key)).map((n) => ({ ...n }));
  for (const level of path) {
    for (const n of answerNodes(level.answer, label)) if (!opened.has(n.key)) nodes.push(n);
  }
  const drawn = new Map(nodes.map((n) => [n.key, n]));
  const edges = new Edges();
  for (const e of base.edges) {
    if (drawn.has(e.source) && drawn.has(e.target))
      for (const p of e.predicates) edges.add(e.source, e.target, p.iri, p.count);
  }
  for (const level of path) {
    for (const e of level.answer.edges) {
      if (!drawn.has(e.source) || !drawn.has(e.target)) continue;
      if (e.source === e.target) {
        const n = drawn.get(e.source)!;
        n.internal = (n.internal ?? 0) + e.count;
        continue;
      }
      edges.add(e.source, e.target, e.predicate, e.count);
    }
  }
  return { nodes, edges: edges.list() };
}

/** A pseudo-random number in [0, 1) from a string, the same for the same string. */
export function hash01(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  // mulberry32 finalizer
  h = Math.imul(h ^ (h >>> 15), h | 1);
  h ^= h + Math.imul(h ^ (h >>> 7), h | 61);
  return ((h ^ (h >>> 14)) >>> 0) / 4294967296;
}

/**
 * A starting position: spread over a disc for the class level, or around the position of
 * the node a group opened from, so the same input gives the same picture.
 */
export function seed(
  key: string,
  around?: { x: number; y: number },
  radius = 100,
): { x: number; y: number } {
  const a = hash01(key) * Math.PI * 2;
  const r = Math.sqrt(hash01(`${key}#r`)) * radius;
  const c = around ?? { x: 0, y: 0 };
  return { x: c.x + Math.cos(a) * r, y: c.y + Math.sin(a) * r };
}

/** The drawn radius of a node of `size` instances when the largest has `max`. */
export function nodeRadius(size: number, max: number, kind: OvKind): number {
  if (kind === 'member') return 4;
  const t = Math.log1p(Math.max(0, size)) / Math.log1p(Math.max(1, max));
  return 5 + 19 * Math.min(1, Math.max(0, t));
}

/** Which renderer draws a graph of `nodes` nodes (spec U06 §2.1). */
export type Renderer = 'cytoscape' | 'webgl';
export const WEBGL_ABOVE = 2000;
export function chooseRenderer(
  nodes: number,
  override: Renderer | null,
  webgl: boolean,
): { renderer: Renderer; reason?: string } {
  if (!webgl) return { renderer: 'cytoscape', reason: 'WebGL is not available in this browser' };
  if (override) return { renderer: override };
  return { renderer: nodes > WEBGL_ABOVE ? 'webgl' : 'cytoscape' };
}

/** Whether the browser offers WebGL 2. */
export function hasWebGL2(): boolean {
  try {
    if (typeof document === 'undefined') return false;
    const c = document.createElement('canvas');
    return !!c.getContext('webgl2');
  } catch {
    return false;
  }
}

/** What the live region says about the picture. */
export function describe(g: OvGraph, path: OvLevel[], fmt: (n: number) => string): string {
  const classes = g.nodes.filter((n) => n.kind === 'class').length;
  if (!path.length) {
    const preds = new Set<string>();
    for (const e of g.edges) for (const p of e.predicates) preds.add(p.iri ?? '');
    return `Class overview: ${fmt(classes)} ${classes === 1 ? 'class' : 'classes'}, ${fmt(preds.size)} ${preds.size === 1 ? 'predicate' : 'predicates'}.`;
  }
  const last = path[path.length - 1];
  const a = last.answer;
  const n = a.grouping.kind === 'members' ? (a.members?.length ?? 0) : a.groups.length;
  const what =
    a.grouping.kind === 'members'
      ? n === 1
        ? 'member'
        : 'members'
      : a.grouping.kind === 'communities'
        ? n === 1
          ? 'community'
          : 'communities'
        : n === 1
          ? 'group'
          : 'groups';
  return `${last.label} opened into ${fmt(n)} ${what}${a.truncated ? ', truncated' : ''}.`;
}

// --- the overview through time (spec U01, Phase 4) ---------------------------------------

const RDF_TYPE_IRI = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';

/** What a grouped diff changed in the class level: instances per class, triples per edge. */
export type OvChanges = {
  /** By class IRI: instances added and removed, from the `rdf:type` changes. */
  classes: Map<string, { added: number; removed: number }>;
  /** By edge key (`c:A\u0001c:B`): triples added and removed, and the net change per predicate. */
  edges: Map<string, { added: number; removed: number; predicates: Map<string, number> }>;
};

/** The class-level changes of a grouped diff. Groups without both classes are left out. */
export function overviewChanges(g: GroupedDiff): OvChanges {
  const out: OvChanges = { classes: new Map(), edges: new Map() };
  for (const x of g.groups) {
    if (!('class' in x.object)) continue;
    const oc = x.object.class;
    if (x.predicate === RDF_TYPE_IRI) {
      if (oc == null) continue;
      const c = out.classes.get(oc) ?? { added: 0, removed: 0 };
      c.added += x.added;
      c.removed += x.removed;
      out.classes.set(oc, c);
      continue;
    }
    if (x.subjectClass == null || oc == null || oc === x.subjectClass) continue;
    const key = `c:${x.subjectClass}\u0001c:${oc}`;
    const e = out.edges.get(key) ?? { added: 0, removed: 0, predicates: new Map() };
    e.added += x.added;
    e.removed += x.removed;
    e.predicates.set(x.predicate, (e.predicates.get(x.predicate) ?? 0) + x.added - x.removed);
    out.edges.set(key, e);
  }
  return out;
}

/**
 * The class level after a step's changes: class sizes move by their instances added and
 * removed, and edges by their triples, so the time-lapse needs no new read of the profiles.
 * A class or an edge that a step adds is drawn, and one that falls to nothing is left out.
 * Nodes keep their keys, so their positions hold still.
 */
export function applyOverviewChanges(
  g: OvGraph,
  c: OvChanges,
  label: (iri: string) => string = (i) => i,
): OvGraph {
  const nodes = g.nodes.map((n) => {
    const d = n.kind === 'class' ? c.classes.get(n.cls) : undefined;
    return d ? { ...n, size: Math.max(0, n.size + d.added - d.removed) } : n;
  });
  const have = new Set(nodes.map((n) => n.key));
  for (const [cls, d] of c.classes) {
    if (have.has(`c:${cls}`) || d.added - d.removed <= 0) continue;
    nodes.push({
      key: `c:${cls}`,
      kind: 'class',
      label: label(cls),
      size: d.added - d.removed,
      cls,
      iri: cls,
      openable: true,
    });
  }
  const kept = nodes.filter((n) => n.size > 0 || n.kind !== 'class');
  const drawn = new Set(kept.map((n) => n.key));
  const edges = new Edges();
  const known = new Set(g.edges.map((e) => e.key));
  for (const e of g.edges) {
    if (!drawn.has(e.source) || !drawn.has(e.target)) continue;
    const d = c.edges.get(e.key);
    const seen = new Set<string | null>();
    for (const p of e.predicates) {
      seen.add(p.iri);
      const n = p.count + (p.iri && d ? (d.predicates.get(p.iri) ?? 0) : 0);
      if (n > 0) edges.add(e.source, e.target, p.iri, n);
    }
    // a predicate the edge did not have yet
    for (const [p, n] of d?.predicates ?? [])
      if (!seen.has(p) && n > 0) edges.add(e.source, e.target, p, n);
  }
  for (const [key, d] of c.edges) {
    const [s, t] = key.split('\u0001');
    if (!drawn.has(s) || !drawn.has(t) || known.has(key)) continue;
    for (const [p, n] of d.predicates) if (n > 0) edges.add(s, t, p, n);
  }
  return { nodes: kept, edges: edges.list() };
}
