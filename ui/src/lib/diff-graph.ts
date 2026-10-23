// The diff graph and the time-lapse (docs/specs/U01-time-slider.md, Phase 4): the changes
// between two commits drawn as a graph of their own, a canvas that applies one step's
// changes at a time, the steps of a range of commits, and the grouped diff drawn as classes.
// Nothing here touches the DOM or the network.
import type { DiffGroup, DiffQuad, GroupedDiff, Term } from './api';
import type { GEdge, GNode } from './components/GraphView.svelte';
import { parseCompact } from './compact';
import { MAX_GHOSTS } from './explore-time';
import { RDF_TYPE, termKey } from './rdf';
import type { Mark } from './rowdiff';

/** The most quads one step moves to the browser. */
export const STEP_QUADS = 500;
/** The most nodes the canvas draws, Explore's cap. */
export const MAX_CANVAS_NODES = 1500;
/** Unchanged edges drawn around each changed resource with **Context**. */
export const CONTEXT_PER_RESOURCE = 20;
/** A range of more commits than this steps through timeline buckets. */
export const BUCKET_ABOVE = 500;
/** The buckets of a long range, so that 100,000 commits take at most 200 steps. */
export const LAPSE_BUCKETS = 200;
/** The steps a second the time-lapse offers. */
export const SPEEDS = [1, 2, 5, 10] as const;
export type Speed = (typeof SPEEDS)[number];

export type DNode = {
  id: string;
  term: Term;
  lit: boolean;
  change: Mark;
  /** The step that last changed it. */
  step: number;
};

export type DEdge = {
  id: string;
  source: string;
  target: string;
  predicate: string;
  lit: boolean;
  change: Mark;
  step: number;
  /** An unchanged edge read around a changed resource, which **context** shows. */
  context?: boolean;
  /** The named graph of the change, in N-Triples; none for the default graph. */
  graph?: string;
};

/** What the canvas draws: elements keyed by id, in the order they were first drawn. */
export type Canvas = {
  nodes: Map<string, DNode>;
  edges: Map<string, DEdge>;
  /** Steps applied so far. */
  step: number;
  /** Additions and removals since the start. */
  added: number;
  removed: number;
};

export function emptyCanvas(): Canvas {
  return { nodes: new Map(), edges: new Map(), step: 0, added: 0, removed: 0 };
}

export type StepResult = {
  canvas: Canvas;
  /** The canvas would pass `maxNodes`: the time-lapse switches to grouping by class. */
  overflow: boolean;
  /** Changes of this step that were applied. */
  added: number;
  removed: number;
  /** Ghosts dropped to stay within `maxGhosts`. */
  droppedGhosts: number;
};

/** The ids the Explore graph gives a quad's ends and edge (a literal gets a node of its own). */
export function quadIds(q: DiffQuad): {
  s: Term;
  o: Term;
  p: string;
  sid: string;
  oid: string;
  edge: string;
  lit: boolean;
} {
  const s = parseCompact(q.subject, {});
  const o = parseCompact(q.object, {});
  const pt = parseCompact(q.predicate, {});
  const p = pt.type === 'uri' ? pt.value : q.predicate;
  const sid = termKey(s);
  const lit = o.type === 'literal';
  const oid = lit ? `lit:${sid}|${p}|${termKey(o)}` : termKey(o);
  return { s, o, p, sid, oid, edge: `${sid}|${p}|${oid}`, lit };
}

function isGhost(x: { change: Mark }) {
  return x.change === 'removed';
}

/**
 * Apply one step's changes to the canvas. Ghosts of earlier steps go unless `keepGhosts`,
 * and what the previous step added is kept. An added quad draws its edge and its ends,
 * next to what is drawn. A removed quad turns its edge into a ghost, drawing it when it was
 * not drawn, and a node whose drawn edges are all ghosts becomes a ghost too. At most
 * `maxGhosts` ghosts stay, the newest first. When the nodes would pass `maxNodes` the step
 * is not applied and `overflow` says so.
 */
export function applyStep(
  prev: Canvas,
  quads: readonly DiffQuad[],
  opts: { keepGhosts?: boolean; maxGhosts?: number; maxNodes?: number } = {},
): StepResult {
  const maxGhosts = opts.maxGhosts ?? MAX_GHOSTS;
  const maxNodes = opts.maxNodes ?? MAX_CANVAS_NODES;
  const step = prev.step + 1;
  const nodes = new Map<string, DNode>();
  const edges = new Map<string, DEdge>();
  for (const [id, n] of prev.nodes) {
    if (isGhost(n) && !opts.keepGhosts) continue;
    nodes.set(id, n.change === 'added' ? { ...n, change: 'kept' } : n);
  }
  for (const [id, e] of prev.edges) {
    if (isGhost(e) && !opts.keepGhosts) continue;
    edges.set(id, e.change === 'added' ? { ...e, change: 'kept' } : e);
  }
  // the nodes this step would add, to stop before the cap
  const fresh = new Set<string>();
  for (const q of quads) {
    if (q.op !== '+') continue;
    const { sid, oid } = quadIds(q);
    for (const id of [sid, oid]) if (!nodes.has(id)) fresh.add(id);
  }
  if (nodes.size + fresh.size > maxNodes)
    return { canvas: prev, overflow: true, added: 0, removed: 0, droppedGhosts: 0 };
  let added = 0;
  let removed = 0;
  const touched = new Set<string>();
  const node = (id: string, term: Term, lit: boolean, change: Mark) => {
    const n = nodes.get(id);
    if (!n) nodes.set(id, { id, term, lit, change, step });
    else if (change === 'added' && isGhost(n)) nodes.set(id, { ...n, change: 'added', step });
  };
  for (const q of quads) {
    const ids = quadIds(q);
    const { s, o, p, sid, oid, lit } = ids;
    // the same triple in two graphs is two changes, so a move between graphs draws both
    const graph = q.graph ?? undefined;
    const edge = graph ? `${ids.edge}|${graph}` : ids.edge;
    if (q.op === '+') {
      added++;
      node(sid, s, false, 'added');
      node(oid, o, lit, 'added');
      edges.set(edge, {
        id: edge,
        source: sid,
        target: oid,
        predicate: p,
        lit,
        change: 'added',
        step,
        ...(graph ? { graph } : {}),
      });
      continue;
    }
    removed++;
    node(sid, s, false, 'removed');
    node(oid, o, lit, 'removed');
    edges.set(edge, {
      id: edge,
      source: sid,
      target: oid,
      predicate: p,
      lit,
      change: 'removed',
      step,
      ...(graph ? { graph } : {}),
    });
    touched.add(sid).add(oid);
  }
  // a node every drawn edge of which is a ghost is a ghost
  const live = new Set<string>();
  for (const e of edges.values()) if (!isGhost(e)) live.add(e.source).add(e.target);
  for (const id of touched) {
    const n = nodes.get(id);
    if (n && !live.has(id) && !isGhost(n)) nodes.set(id, { ...n, change: 'removed', step });
  }
  // the newest ghosts stay
  const ghosts = [
    ...[...nodes.values()]
      .filter(isGhost)
      .map((n) => ({ kind: 'n' as const, id: n.id, step: n.step })),
    ...[...edges.values()]
      .filter(isGhost)
      .map((e) => ({ kind: 'e' as const, id: e.id, step: e.step })),
  ];
  let droppedGhosts = 0;
  if (ghosts.length > maxGhosts) {
    ghosts.sort((a, b) => b.step - a.step);
    for (const g of ghosts.slice(maxGhosts)) {
      if (g.kind === 'n') nodes.delete(g.id);
      else edges.delete(g.id);
      droppedGhosts++;
    }
    // an edge whose end is gone goes with it
    for (const [id, e] of edges) if (!nodes.has(e.source) || !nodes.has(e.target)) edges.delete(id);
  }
  return {
    canvas: {
      nodes,
      edges,
      step,
      added: prev.added + added,
      removed: prev.removed + removed,
    },
    overflow: false,
    added,
    removed,
    droppedGhosts,
  };
}

/** The canvas of one diff: its quads on an empty canvas, ghosts kept. */
export function diffCanvas(quads: readonly DiffQuad[], maxNodes = MAX_CANVAS_NODES): StepResult {
  return applyStep(emptyCanvas(), quads, {
    keepGhosts: true,
    maxGhosts: Number.POSITIVE_INFINITY,
    maxNodes,
  });
}

/** An unchanged edge around a changed resource, from the `neighbours` read at the `to` state. */
export type ContextEdge = { s: Term; p: string; o: Term };

/**
 * The canvas with context: the unchanged edges of each changed resource, up to `per` for
 * each, drawn as kept, and their other ends. Changed elements keep their marks.
 */
export function withContext(
  c: Canvas,
  context: ReadonlyMap<string, readonly ContextEdge[]>,
  per = CONTEXT_PER_RESOURCE,
  maxNodes = MAX_CANVAS_NODES,
): Canvas {
  const nodes = new Map(c.nodes);
  const edges = new Map(c.edges);
  // a change keeps its graph in its id, and context is read without graphs: a changed
  // triple is not drawn again as context
  const changed = new Set(
    [...c.edges.values()].map((e) =>
      e.graph ? e.id.slice(0, e.id.length - e.graph.length - 1) : e.id,
    ),
  );
  for (const [, list] of context) {
    let n = 0;
    for (const { s, p, o } of list) {
      if (n >= per) break;
      const q: DiffQuad = {
        op: '+',
        subject: nt(s),
        predicate: `<${p}>`,
        object: nt(o),
        graph: null,
      };
      const ids = quadIds(q);
      if (edges.has(ids.edge) || changed.has(ids.edge)) continue;
      const fresh = [ids.sid, ids.oid].filter((id) => !nodes.has(id)).length;
      if (nodes.size + fresh > maxNodes) break;
      if (!nodes.has(ids.sid))
        nodes.set(ids.sid, { id: ids.sid, term: s, lit: false, change: 'kept', step: c.step });
      if (!nodes.has(ids.oid))
        nodes.set(ids.oid, { id: ids.oid, term: o, lit: ids.lit, change: 'kept', step: c.step });
      edges.set(ids.edge, {
        id: ids.edge,
        source: ids.sid,
        target: ids.oid,
        predicate: p,
        lit: ids.lit,
        change: 'kept',
        step: c.step,
        context: true,
      });
      n++;
    }
  }
  return { ...c, nodes, edges };
}

/** A term in N-Triples, as the diff gives it. */
function nt(t: Term): string {
  if (t.type === 'uri') return `<${t.value}>`;
  if (t.type === 'bnode') return `_:${t.value}`;
  if (t.type === 'literal') {
    const v = JSON.stringify(t.value);
    if (t['xml:lang']) return `${v}@${t['xml:lang']}`;
    if (t.datatype && t.datatype !== 'http://www.w3.org/2001/XMLSchema#string')
      return `${v}^^<${t.datatype}>`;
    return v;
  }
  return termKey(t);
}

/** The IRIs of the resources a step changed, for the `neighbours` reads of **Context**. */
export function changedResources(quads: readonly DiffQuad[], max = 50): string[] {
  const out = new Set<string>();
  for (const q of quads) {
    for (const x of [q.subject, q.object]) {
      const t = parseCompact(x, {});
      if (t.type === 'uri') out.add(t.value);
      if (out.size >= max) return [...out];
    }
  }
  return [...out];
}

export type Shown = { added: boolean; removed: boolean; context: boolean; literals: boolean };

/** The canvas as GraphView elements, with the marks the person chose to see. */
export function canvasElements(
  c: Canvas,
  shown: Shown,
  label: (t: Term) => string,
  short: (iri: string) => string,
): { nodes: GNode[]; edges: GEdge[] } {
  const keepEdge = (e: DEdge) =>
    (shown.literals || !e.lit) &&
    (e.change === 'added'
      ? shown.added
      : e.change === 'removed'
        ? shown.removed
        : // what an earlier step of a time-lapse drew stays, context only with context
          !e.context || shown.context);
  const edges = [...c.edges.values()].filter(keepEdge);
  const ends = new Set(edges.flatMap((e) => [e.source, e.target]));
  const nodes = [...c.nodes.values()].filter(
    (n) =>
      ends.has(n.id) && (shown.literals || !n.lit) && (n.change !== 'removed' || shown.removed),
  );
  const drawn = new Set(nodes.map((n) => n.id));
  return {
    nodes: nodes.map((n) => ({
      id: n.id,
      label: label(n.term),
      kind: n.term.type === 'uri' ? 'iri' : n.term.type,
      title: n.term.type === 'uri' ? n.term.value : undefined,
      change: n.change === 'kept' ? undefined : n.change,
    })),
    edges: edges
      .filter((e) => drawn.has(e.source) && drawn.has(e.target))
      .map((e) => ({
        id: e.id,
        source: e.source,
        target: e.target,
        label: short(e.predicate),
        title: e.graph ? `${e.predicate} in ${e.graph}` : e.predicate,
        isType: e.predicate === RDF_TYPE,
        change: e.change === 'kept' ? undefined : e.change,
      })),
  };
}

/** The changes of a step as rows for the list view: `+` or `−` and the quad. */
export function changeRows(
  quads: readonly DiffQuad[],
): { sign: '+' | '−'; s: string; p: string; o: string; g: string | null }[] {
  return quads.map((q) => ({
    sign: q.op === '+' ? '+' : '−',
    s: q.subject,
    p: q.predicate,
    o: q.object,
    g: q.graph,
  }));
}

// --- the steps of a time-lapse ----------------------------------------------------------

/**
 * One step: the changes after commit `from` through commit `to`, and how many changes the
 * commit catalog counts for them, when it says (not for a caller limited to some graphs).
 */
export type LapseStep = { from: number; to: number; changes?: number };

/** A bucket of the timeline route: its first and last commit, and its counts. */
export type BucketSpan = { firstSeq: number; lastSeq: number; inserted?: number; deleted?: number };

/**
 * The steps from commit `from` to commit `to`: one per commit up to `BUCKET_ABOVE` commits,
 * one per timeline bucket beyond (`buckets`, in order), clipped to the range.
 */
export function lapseSteps(from: number, to: number, buckets?: readonly BucketSpan[]): LapseStep[] {
  if (to <= from) return [];
  if (to - from <= BUCKET_ABOVE || !buckets?.length) {
    const out: LapseStep[] = [];
    // without buckets a long range is cut into even spans
    const width = to - from <= BUCKET_ABOVE ? 1 : Math.ceil((to - from) / LAPSE_BUCKETS);
    for (let s = from; s < to; s += width) out.push({ from: s, to: Math.min(to, s + width) });
    return out;
  }
  const out: LapseStep[] = [];
  let at = from;
  for (const b of buckets) {
    const end = Math.min(b.lastSeq, to);
    if (end <= at) continue;
    const counted = b.inserted != null && b.deleted != null && b.firstSeq > at && b.lastSeq <= to;
    out.push({ from: at, to: end, ...(counted ? { changes: b.inserted! + b.deleted! } : {}) });
    at = end;
    if (at >= to) break;
  }
  if (at < to) out.push({ from: at, to });
  return out;
}

/**
 * The steps with the counts of their commits from the catalog. A step whose count passes
 * `STEP_QUADS` is read as a grouped diff at once, with no request for quads it could not
 * draw.
 */
export function withCounts(
  steps: readonly LapseStep[],
  commits: readonly { seq: number; inserted?: number; deleted?: number }[],
): LapseStep[] {
  const by = new Map(commits.map((c) => [c.seq, c]));
  return steps.map((s) => {
    if (s.changes != null || s.to - s.from !== 1) return s;
    const c = by.get(s.to);
    return c?.inserted != null && c.deleted != null ? { ...s, changes: c.inserted + c.deleted } : s;
  });
}

/** Whether a step is too large to draw as quads. */
export const tooLarge = (s: LapseStep) => s.changes != null && s.changes > STEP_QUADS;

/** The delay before the next step at `speed` steps a second, never before the last one ended. */
export function stepDelay(speed: Speed, lastStepMs: number): number {
  return Math.max(0, 1000 / speed - lastStepMs);
}

/** What the live region says after a step: "commit 241, 12 added, 1 removed". */
export function stepAnnouncement(s: LapseStep, added: number, removed: number): string {
  const which = s.to - s.from === 1 ? `commit ${s.to}` : `commits ${s.from + 1} to ${s.to}`;
  return `${which}, ${added.toLocaleString('en-US')} added, ${removed.toLocaleString('en-US')} removed`;
}

// --- the grouped diff as a graph --------------------------------------------------------

/** The node key of a group's subject or object. */
function classKey(c: string | null): string {
  return c == null ? 'c:' : `c:${c}`;
}

function objectKey(g: DiffGroup): string {
  const o = g.object;
  if ('literal' in o) return `l:${g.subjectClass ?? ''}|${o.literal}`;
  if ('iri' in o) return 'i:';
  return classKey(o.class);
}

/** A mark from counts: added, removed, or both. */
export function markOf(added: number, removed: number): Mark | undefined {
  if (added && !removed) return 'added';
  if (removed && !added) return 'removed';
  return added || removed ? 'kept' : undefined;
}

/** `+600`, `−4`, or `+3 −1`. */
export function countsText(added: number, removed: number): string {
  const f = (n: number) => n.toLocaleString('en-US');
  return [added ? `+${f(added)}` : '', removed ? `−${f(removed)}` : ''].filter(Boolean).join(' ');
}

export type ClassElements = {
  nodes: (GNode & { cls: string | null; added: number; removed: number })[];
  edges: (GEdge & { added: number; removed: number })[];
  /** Groups left out because their nodes would pass `maxNodes`. */
  omitted: number;
};

/**
 * The grouped diff drawn as classes: a node per class with the instances its `rdf:type`
 * changes added and removed, a node per datatype of each class's literals and one for IRIs
 * without a class, and an edge per predicate between them labelled with its changes.
 */
export function groupedElements(
  g: Pick<GroupedDiff, 'groups'>,
  short: (iri: string) => string,
  maxNodes = MAX_CANVAS_NODES,
): ClassElements {
  const nodes = new Map<string, ClassElements['nodes'][number]>();
  const edges = new Map<string, ClassElements['edges'][number]>();
  const ensure = (
    key: string,
    label: string,
    cls: string | null,
    kind: GNode['kind'],
    title?: string,
  ) => {
    let n = nodes.get(key);
    if (!n) {
      n = { id: key, label, kind, title, cls, added: 0, removed: 0 };
      nodes.set(key, n);
    }
    return n;
  };
  const classNode = (c: string | null) =>
    ensure(classKey(c), c == null ? 'No class' : short(c), c, 'iri', c ?? undefined);
  // the largest groups first, and no group whose nodes would pass the cap
  const order = [...g.groups].sort((a, b) => b.added + b.removed - (a.added + a.removed));
  let omitted = 0;
  for (const x of order) {
    const fresh = new Set([classKey(x.subjectClass), objectKey(x)].filter((k) => !nodes.has(k)));
    if (nodes.size + fresh.size > maxNodes) {
      omitted++;
      continue;
    }
    const s = classNode(x.subjectClass);
    if (x.predicate === RDF_TYPE && 'class' in x.object) {
      // instances that gained or lost a class
      const c = classNode(x.object.class);
      c.added += x.added;
      c.removed += x.removed;
      if (x.object.class === x.subjectClass) continue;
      void s;
    }
    const o = x.object;
    const ok = objectKey(x);
    if ('literal' in o) ensure(ok, short(o.literal), x.subjectClass, 'literal', o.literal);
    else if ('iri' in o) ensure(ok, 'IRIs without a class', null, 'iri');
    else classNode(o.class);
    const id = `${s.id}|${x.predicate}|${ok}`;
    let e = edges.get(id);
    if (!e) {
      e = { id, source: s.id, target: ok, label: '', title: x.predicate, added: 0, removed: 0 };
      edges.set(id, e);
    }
    e.added += x.added;
    e.removed += x.removed;
  }
  for (const n of nodes.values()) {
    const c = countsText(n.added, n.removed);
    n.label = c ? `${n.label} ${c}` : n.label;
    n.change = markOf(n.added, n.removed);
  }
  for (const e of edges.values()) {
    e.label = `${short(e.title ?? '')} ${countsText(e.added, e.removed)}`.trim();
    e.change = markOf(e.added, e.removed);
    e.isType = e.title === RDF_TYPE;
  }
  return { nodes: [...nodes.values()], edges: [...edges.values()], omitted };
}

/** The key of a group: its subject class, predicate and object. */
export function groupKeyOf(g: DiffGroup): string {
  return `${g.subjectClass}|${g.predicate}|${JSON.stringify(g.object)}`;
}

/**
 * A group a grouped time-lapse has drawn, with the triples of it the time-lapse has seen
 * added and not removed since. It never goes below nothing, so removing triples that were
 * there before the start does not cancel later additions.
 */
export type LiveGroup = { group: DiffGroup; net: number; lastRemoved: number };

/**
 * One step of a grouped time-lapse. Each group keeps its live count (`LiveGroup`). A
 * group the step changed carries the step's counts, so it is marked as the canvas marks a
 * step, one with a positive net change stays unmarked, and one that fell to nothing or below
 * goes after its step unless `keepGhosts`, then stays as a ghost. At most `max` groups are
 * kept, ghosts going first.
 */
export function lapseGroupsStep(
  live: ReadonlyMap<string, LiveGroup>,
  step: readonly DiffGroup[],
  keepGhosts: boolean,
  max = 5000,
): { live: Map<string, LiveGroup>; groups: DiffGroup[] } {
  const next = new Map(live);
  const changed = new Map<string, DiffGroup>();
  for (const x of step) {
    const k = groupKeyOf(x);
    const prev = next.get(k);
    next.set(k, {
      group: x,
      net: Math.max(0, (prev?.net ?? 0) + x.added - x.removed),
      lastRemoved: x.removed || (prev?.lastRemoved ?? 0),
    });
    changed.set(k, x);
  }
  const groups: DiffGroup[] = [];
  for (const [k, v] of next) {
    // a group at nothing goes after its step unless ghosts are kept
    const c = changed.get(k);
    if (c) groups.push(c);
    else if (v.net > 0) groups.push({ ...v.group, added: 0, removed: 0 });
    else if (keepGhosts) groups.push({ ...v.group, added: 0, removed: Math.max(1, v.lastRemoved) });
    else next.delete(k);
  }
  if (next.size > max) {
    const drop = [...next.entries()]
      .sort((a, b) => Number(a[1].net > 0) - Number(b[1].net > 0) || a[1].net - b[1].net)
      .slice(0, next.size - max)
      .map(([k]) => k);
    const gone = new Set(drop);
    for (const k of drop) next.delete(k);
    return { live: next, groups: groups.filter((x) => !gone.has(groupKeyOf(x))) };
  }
  return { live: next, groups };
}

/** The class a node of the grouped graph stands for, to open it into its members. */
export function classOfKey(key: string): string | null | undefined {
  if (!key.startsWith('c:')) return undefined;
  return key.length > 2 ? key.slice(2) : null;
}

// --- links ------------------------------------------------------------------------------

/** The Explore address of the diff graph from `from` to `to` (selectors of `at`). */
export function diffGraphHref(
  base: string,
  ds: string,
  to: string,
  from?: string,
  opts: { branch?: string | null; group?: boolean } = {},
): string {
  const p = new URLSearchParams();
  p.set('ds', ds);
  p.set('diff', to);
  if (from) p.set('diffFrom', from);
  if (opts.branch && opts.branch !== 'main') p.set('diffBranch', opts.branch);
  if (opts.group) p.set('group', 'class');
  return `${base}?${p}`;
}

/** The commit number of a `commit:N` or `N` selector, or null. */
export function seqOf(sel: string | null | undefined): number | null {
  if (sel == null) return null;
  const m = /^(?:commit:)?(\d+)$/.exec(sel.trim());
  return m ? Number(m[1]) : null;
}
