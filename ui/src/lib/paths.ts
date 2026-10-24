// The path finder of Explore (docs/specs/U09-path-finder.md): the query the panel runs
// through `SERVICE path:search`, the rows grouped into paths, and the overlay the graph
// draws from them.
import * as api from './api';
import type { Term } from './api';
import { RDF_TYPE, WELL_KNOWN, localName, sparqlIri, termKey, toSparql } from './rdf';

export type PathMode = 'shortest' | 'allShortest' | 'kShortest';
export type PathDirection = 'forward' | 'backward' | 'both';

export const OWL_SAME_AS = WELL_KNOWN.owl + 'sameAs';
/** The predicates skipped by default: every entity is close to every other through them. */
export const DEFAULT_EXCLUDED = [RDF_TYPE, OWL_SAME_AS];
/** The longest path the panel offers. */
export const MAX_LENGTH = 8;
/** All shortest paths stop at this many. */
export const ALL_SHORTEST_CAP = 50;
/** A tenth of F07's default, so a search from the UI stops long before the server's limit. */
export const MAX_VISITED = 1_000_000;
/** Seconds a search may take. */
export const TIMEOUT_S = 10;
/** The hub limits the panel offers; null turns the limit off. */
export const HUB_LIMITS: (number | null)[] = [100, 1000, 10_000, 100_000, null];

export type PathSettings = {
  source: Term;
  target: Term;
  mode: PathMode;
  /** Paths in `kShortest`, from 2 to 20. */
  k: number;
  /** The fewest edges, used by `kShortest` only. */
  minLength: number;
  maxLength: number;
  direction: PathDirection;
  /** Do not pass through a node with more edges than this; null for no limit. */
  maxDegree: number | null;
  /** Follow every predicate except `predicates`, or only `predicates`. */
  follow: 'except' | 'only';
  predicates: string[];
  /**
   * The property of an edge's reifier that holds its weight (U09 Phase 3), or null to count
   * edges. An edge without one weighs 1.
   */
  weight?: string | null;
};

export function defaultSettings(source: Term, target: Term): PathSettings {
  return {
    source,
    target,
    mode: 'shortest',
    k: 3,
    minLength: 1,
    maxLength: 6,
    direction: 'both',
    maxDegree: 1000,
    follow: 'except',
    predicates: [...DEFAULT_EXCLUDED],
  };
}

/** The most paths a search returns in each mode. */
export function pathLimit(s: Pick<PathSettings, 'mode' | 'k'>): number {
  return s.mode === 'shortest' ? 1 : s.mode === 'allShortest' ? ALL_SHORTEST_CAP : s.k;
}

/**
 * What stops the search from running, or null. A literal can end a path but not start one,
 * and a path needs two different ends.
 */
export function invalidReason(s: PathSettings): string | null {
  if (s.source.type === 'literal') return 'A path cannot start at a literal';
  if (s.source.type !== 'uri' && s.source.type !== 'bnode') return 'Choose where the path starts';
  if (termKey(s.source) === termKey(s.target)) return 'Choose two different entities';
  if (s.mode === 'kShortest' && s.minLength > s.maxLength)
    return 'The shortest length is above the longest';
  return null;
}

/**
 * The query of a search. `phase1` is false on a server without `path:excludePredicate` and
 * `path:maxDegree`: the search then follows the listed predicates, or every predicate when
 * the list skips them.
 */
export function pathQuery(s: PathSettings, phase1 = true): string {
  const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, Math.round(n)));
  const max = clamp(s.maxLength, 1, MAX_LENGTH);
  const lines = [
    `path:source ${toSparql(s.source)} ; path:target ${toSparql(s.target)} ;`,
    `path:algorithm path:${s.mode} ;`,
  ];
  if (s.mode === 'kShortest') {
    const k = clamp(s.k, 2, 20);
    const min = clamp(s.minLength, 1, max);
    lines.push(`path:k ${k} ;${min > 1 ? ` path:minLength ${min} ;` : ''} path:maxLength ${max} ;`);
  } else {
    lines.push(`path:maxLength ${max} ;`);
  }
  lines.push(`path:direction path:${s.direction} ;`);
  if (phase1 && s.maxDegree != null) lines.push(`path:maxDegree ${Math.max(1, s.maxDegree)} ;`);
  const preds = [...new Set(s.predicates)].map(sparqlIri);
  if (preds.length && s.follow === 'only') lines.push(`path:predicate ${preds.join(' , ')} ;`);
  if (preds.length && s.follow === 'except' && phase1)
    lines.push(`path:excludePredicate ${preds.join(' , ')} ;`);
  if (s.weight) lines.push(`path:weight ${sparqlIri(s.weight)} ;`);
  lines.push(`path:maxVisited ${MAX_VISITED} ; path:limit ${pathLimit(s)} ;`);
  lines.push(
    'path:pathIndex ?path ; path:edgeIndex ?i ;',
    `path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o ; path:length ?len${s.weight ? ' ; path:cost ?cost' : ''} .`,
  );
  return `PREFIX path: <urn:x-sparkles:path#>
SELECT ?path ?i ?s ?p ?o ?len${s.weight ? ' ?cost' : ''} WHERE {
  SERVICE path:search {
    [] ${lines.join('\n       ')}
  }
}
ORDER BY ?path ?i
`;
}

export type PathEdge = {
  /** The stored triple. */
  s: Term;
  p: string;
  o: Term;
  /** The node before and after the edge on the path (keys). */
  from: string;
  to: string;
  /** The triple runs along the path, from its subject to its object. */
  forward: boolean;
  /** The id the Explore graph gives the triple's edge. */
  id: string;
};

export type FoundPath = {
  /** 1 for the first path. */
  n: number;
  length: number;
  /** The sum of the edges' weights, when the search had weights. */
  cost?: number;
  nodes: Term[];
  edges: PathEdge[];
};

export const edgeId = (s: Term, p: string, o: Term) => `${termKey(s)}|${p}|${termKey(o)}`;

type Row = Record<string, Term | undefined>;

const num = (t: Term | undefined) => (t && t.type === 'literal' ? Number(t.value) : NaN);

/** The rows of a search as paths, in order, each edge oriented from `source`. */
export function groupPaths(rows: Row[], source: Term): FoundPath[] {
  const by = new Map<number, { len: number; cost: number; edges: Map<number, Row> }>();
  for (const r of rows) {
    const k = num(r.path);
    if (!Number.isFinite(k)) continue;
    let g = by.get(k);
    if (!g) by.set(k, (g = { len: num(r.len), cost: num(r.cost), edges: new Map() }));
    const i = num(r.i);
    if (Number.isFinite(i) && r.s && r.p && r.o) g.edges.set(i, r);
  }
  const out: FoundPath[] = [];
  for (const k of [...by.keys()].sort((a, b) => a - b)) {
    const g = by.get(k)!;
    let at = source;
    const nodes: Term[] = [source];
    const edges: PathEdge[] = [];
    for (const i of [...g.edges.keys()].sort((a, b) => a - b)) {
      const r = g.edges.get(i)!;
      const s = r.s!;
      const o = r.o!;
      const p = r.p!.value as string;
      const forward = termKey(s) === termKey(at);
      const next = forward ? o : s;
      edges.push({ s, p, o, from: termKey(at), to: termKey(next), forward, id: edgeId(s, p, o) });
      nodes.push(next);
      at = next;
    }
    out.push({
      n: out.length + 1,
      length: Number.isFinite(g.len) ? g.len : edges.length,
      ...(Number.isFinite(g.cost) ? { cost: g.cost } : {}),
      nodes,
      edges,
    });
  }
  return out;
}

export type OverlayNode = { key: string; term: Term; paths: number[]; order: number };
export type OverlayEdge = {
  id: string;
  s: Term;
  p: string;
  o: Term;
  paths: number[];
  /** When the edge is drawn: 0 first, in path order, then edge order. */
  order: number;
};

/**
 * What the graph draws for the paths: each node and edge once, with the numbers of every
 * path through it, and the order in which the animation draws them. A node is drawn with
 * the first edge that reaches it, the source first of all.
 */
export function overlay(paths: FoundPath[]): { nodes: OverlayNode[]; edges: OverlayEdge[] } {
  const nodes = new Map<string, OverlayNode>();
  const edges = new Map<string, OverlayEdge>();
  let step = 0;
  for (const path of paths) {
    const first = path.nodes[0];
    if (first) {
      const k = termKey(first);
      const n = nodes.get(k);
      if (n) n.paths.push(path.n);
      else nodes.set(k, { key: k, term: first, paths: [path.n], order: 0 });
    }
    path.edges.forEach((e, i) => {
      let ed = edges.get(e.id);
      if (ed) {
        if (!ed.paths.includes(path.n)) ed.paths.push(path.n);
      } else {
        step++;
        edges.set(e.id, (ed = { id: e.id, s: e.s, p: e.p, o: e.o, paths: [path.n], order: step }));
      }
      const t = path.nodes[i + 1];
      const k = termKey(t);
      const n = nodes.get(k);
      if (n) {
        if (!n.paths.includes(path.n)) n.paths.push(path.n);
      } else nodes.set(k, { key: k, term: t, paths: [path.n], order: ed.order });
    });
  }
  return { nodes: [...nodes.values()], edges: [...edges.values()] };
}

/** "works for" from `ex:worksFor`, "located in" from `…/located_in`. */
export function predicateWords(iri: string): string {
  const name = localName(iri) || iri;
  return name
    .replace(/[_-]+/g, ' ')
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .toLowerCase()
    .trim();
}

/** A cost as the list shows it: at most three decimals, without trailing zeros. */
export function fmtCost(n: number): string {
  return Number.isInteger(n) ? String(n) : String(Number(n.toFixed(3)));
}

/** A path in words, for its row in the list and for screen readers. */
export function describePath(path: FoundPath, label: (t: Term) => string): string {
  const parts = path.edges.map((e) => `${label(e.s)} ${predicateWords(e.p)} ${label(e.o)}`);
  const cost = path.cost != null ? [`cost ${fmtCost(path.cost)}`] : [];
  return [`path ${path.n}`, `length ${path.length}`, ...cost, ...parts].join(', ');
}

/** What the live region says after a search. */
export function announcement(paths: FoundPath[]): string {
  if (!paths.length) return 'No path found';
  const found = `${paths.length} ${paths.length === 1 ? 'path' : 'paths'} found`;
  const costs = paths.flatMap((p) => (p.cost != null ? [p.cost] : []));
  if (costs.length === paths.length)
    return `${found}, the cheapest of cost ${fmtCost(Math.min(...costs))}`;
  const shortest = Math.min(...paths.map((p) => p.length));
  return `${found}, the shortest of length ${shortest}`;
}

/** A property that holds edge weights on reifiers, with the number of values it has. */
export type WeightProperty = { iri: string; count: number };

/**
 * The query that lists the weight properties: properties with numeric values on the
 * reifiers of the caller's view, the most used first.
 */
export function weightQuery(limit = 20): string {
  return `PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
SELECT ?w (COUNT(*) AS ?n) WHERE {
  ?r rdf:reifies ?t .
  ?r ?w ?v .
  FILTER(isNumeric(?v) && ?v >= 0)
}
GROUP BY ?w
ORDER BY DESC(?n) ?w
LIMIT ${limit}
`;
}

/** The weight properties of a dataset at a state, for the weight picker. */
export async function weightProperties(
  ds: string,
  opts: { at?: string; signal?: AbortSignal } = {},
): Promise<WeightProperty[]> {
  const r = await api.query(ds, weightQuery(), {
    timeout: TIMEOUT_S,
    send: 100,
    ...(opts.at ? { at: opts.at } : {}),
    signal: opts.signal,
  });
  const vars = r.vars ?? [];
  const wi = vars.indexOf('w');
  const ni = vars.indexOf('n');
  const out: WeightProperty[] = [];
  for (const row of r.rows ?? []) {
    const w = row[wi];
    if (w?.type !== 'uri') continue;
    out.push({ iri: w.value, count: num(row[ni] ?? undefined) || 0 });
  }
  return out;
}

export type SearchFailure = {
  kind: 'visited' | 'timeout' | 'unsupported' | 'other';
  message: string;
};

/** What a failed search means for the panel. */
export function classifyFailure(e: unknown): SearchFailure {
  const msg = e instanceof Error ? e.message : String(e);
  const detail = e instanceof api.ApiError ? `${msg} ${e.detail ?? ''}` : msg;
  if (/unknown parameter path:(excludePredicate|maxDegree)/.test(detail))
    return {
      kind: 'unsupported',
      message:
        'This server cannot skip predicates or hubs. Choose the predicates to follow and search again.',
    };
  if (/visited more than/.test(detail))
    return {
      kind: 'visited',
      message: `The search stopped after visiting ${MAX_VISITED.toLocaleString('en-US')} nodes. Skip more predicates or lower the hub limit.`,
    };
  if ((e instanceof api.ApiError && e.status === 408) || /timeout|did not finish/i.test(detail))
    return {
      kind: 'timeout',
      message: `The search took longer than ${TIMEOUT_S} s. Skip more predicates or lower the hub limit.`,
    };
  return { kind: 'other', message: msg };
}

export type PathResult = {
  paths: FoundPath[];
  rows: number;
  commit?: number;
  ms: number;
  visited?: number;
  skippedHubs?: number;
};

function counters(p: api.PlanNode | undefined): Record<string, unknown> | undefined {
  if (!p) return undefined;
  if (p.operator === 'PathSearch') return p.counters;
  for (const c of p.children ?? []) {
    const f = counters(c);
    if (f) return f;
  }
  return undefined;
}

/** Run a search, at Explore's state. */
export async function findPaths(
  ds: string,
  s: PathSettings,
  opts: { at?: string; phase1?: boolean; signal?: AbortSignal } = {},
): Promise<PathResult> {
  const r = await api.query(ds, pathQuery(s, opts.phase1 ?? true), {
    timeout: TIMEOUT_S,
    send: 1000,
    ...(opts.at ? { at: opts.at } : {}),
    signal: opts.signal,
  });
  const vars = r.vars ?? [];
  const rows = (r.rows ?? []).map((row) =>
    Object.fromEntries(vars.map((v, i) => [v, row[i] ?? undefined])),
  );
  const c = counters(r.meta?.plan);
  const n = (x: unknown) => (typeof x === 'number' ? x : undefined);
  return {
    paths: groupPaths(rows, s.source),
    rows: rows.length,
    commit: r.meta?.commit,
    ms: r.meta?.timing?.totalMs ?? 0,
    visited: n(c?.visited),
    skippedHubs: n(c?.skippedHubs),
  };
}
