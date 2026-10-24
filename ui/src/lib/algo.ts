// The algorithm lenses (docs/specs/U10-canvas-lenses.md §2.3 and §3.2): F14 algorithms run
// as view-only runs, either over the drawn graph through `SERVICE algo:` on `/{ds}/sparql`,
// or over a projection of the dataset through `POST /$/algo/{ds}/run` with `nodes` and
// `keep`, in the background with progress when the run takes a while. Nothing is written.
import * as api from './api';
import type { Term } from './api';
import { sparqlIri } from './rdf';
import { chunk, type Limiter } from './lens-sources';

export const ALGO_NS = 'urn:x-sparkles:algo#';
/** Drawn nodes a drawn-graph run takes at most (§5). */
export const MAX_DRAWN = 1_500;
/** Nodes one run request may list (§3.2). */
export const MAX_RUN_NODES = 20_000;
/** Nodes a numeric lens shows above `MAX_RUN_NODES` drawn nodes (§5). */
export const TOP = 1_000;

export type AlgoId =
  | 'pageRank'
  | 'betweenness'
  | 'bfs'
  | 'kCore'
  | 'triangleCount'
  | 'wcc'
  | 'labelPropagation'
  | 'louvain'
  | 'leiden';

export type ParamSpec = {
  key: string;
  name: string;
  default: number;
  min?: number;
  max?: number;
  step?: number;
  /** Sent only when it differs from 0, such as betweenness samples (0 is all nodes). */
  zeroIsUnset?: boolean;
};

export type AlgoSpec = {
  id: AlgoId;
  channel: 'size' | 'colour';
  name: string;
  /** The output the lens shows. */
  output: string;
  params: ParamSpec[];
  /** Has `level` rows, one per node and level. */
  hierarchical?: boolean;
  /** Needs a start node, `algo:source`. */
  needsSource?: boolean;
};

export const ALGOS: Record<AlgoId, AlgoSpec> = {
  pageRank: {
    id: 'pageRank',
    channel: 'size',
    name: 'PageRank',
    output: 'score',
    params: [
      { key: 'dampingFactor', name: 'damping', default: 0.85, min: 0, max: 1, step: 0.05 },
      { key: 'maxIterations', name: 'iterations', default: 20, min: 1, max: 1000, step: 1 },
    ],
  },
  betweenness: {
    id: 'betweenness',
    channel: 'size',
    name: 'betweenness',
    output: 'score',
    params: [
      { key: 'samples', name: 'samples', default: 0, min: 0, step: 1, zeroIsUnset: true },
      { key: 'seed', name: 'seed', default: 42, min: 0, step: 1 },
    ],
  },
  bfs: {
    id: 'bfs',
    channel: 'size',
    name: 'BFS depth from a node',
    output: 'depth',
    params: [],
    needsSource: true,
  },
  kCore: { id: 'kCore', channel: 'size', name: 'k-core', output: 'core', params: [] },
  triangleCount: {
    id: 'triangleCount',
    channel: 'size',
    name: 'triangles',
    output: 'triangles',
    params: [],
  },
  wcc: { id: 'wcc', channel: 'colour', name: 'WCC components', output: 'component', params: [] },
  labelPropagation: {
    id: 'labelPropagation',
    channel: 'colour',
    name: 'label propagation',
    output: 'community',
    params: [{ key: 'maxIterations', name: 'iterations', default: 10, min: 1, max: 1000, step: 1 }],
  },
  louvain: {
    id: 'louvain',
    channel: 'colour',
    name: 'Louvain communities',
    output: 'community',
    params: [{ key: 'resolution', name: 'resolution', default: 1, min: 0, step: 0.1 }],
    hierarchical: true,
  },
  leiden: {
    id: 'leiden',
    channel: 'colour',
    name: 'Leiden communities',
    output: 'community',
    params: [
      { key: 'resolution', name: 'resolution', default: 1, min: 0, step: 0.1 },
      { key: 'seed', name: 'seed', default: 42, min: 0, step: 1 },
    ],
    hierarchical: true,
  },
};

export const isAlgo = (id: string): id is AlgoId => id in ALGOS;

/** The parameters a run sends: the given ones, then the defaults, without unset ones. */
export function runParams(
  spec: AlgoSpec,
  given: Record<string, number> = {},
  source?: string,
): Record<string, number | string> {
  const out: Record<string, number | string> = {};
  for (const p of spec.params) {
    const v = given[p.key] ?? p.default;
    if (!Number.isFinite(v) || (p.zeroIsUnset && v === 0)) continue;
    out[p.key] = v;
  }
  if (spec.needsSource && source) out.source = source;
  return out;
}

const literal = (v: number | string) =>
  typeof v === 'number'
    ? Number.isInteger(v)
      ? String(v)
      : String(v).includes('e')
        ? v.toExponential()
        : String(v)
    : sparqlIri(v);

/**
 * The drawn-graph query (§3.2): the algorithm over the drawn nodes and the edges among them,
 * through a nested pattern whose `VALUES` blocks both hold the drawn nodes. An `rdf:type`
 * edge is in it only when its class is drawn too.
 */
export function drawnQuery(
  spec: AlgoSpec,
  iris: readonly string[],
  params: Record<string, number | string>,
): string {
  const vals = iris.map(sparqlIri).join(' ');
  const outs = ['node', spec.output, ...(spec.hierarchical ? ['level'] : []), 'summary'];
  const parts = [
    'algo:start ?s',
    'algo:end ?o',
    ...outs.map((o) => `algo:${o} ?${o}`),
    ...Object.entries(params).map(([k, v]) => `algo:${k} ${literal(v)}`),
  ];
  return `PREFIX algo: <${ALGO_NS}>
SELECT ${outs.map((o) => `?${o}`).join(' ')} WHERE {
  SERVICE algo:${spec.id} {
    [] ${parts.join(' ; ')} .
    VALUES ?s { ${vals} }
    VALUES ?o { ${vals} }
    ?s ?p ?o .
  }
}`;
}

export type AlgoRows = {
  /** Node IRI to its value at the chosen level. */
  values: Map<string, number | string>;
  /** The size of each community or component in the whole projection, by value. */
  sizes: Map<string, number>;
  /** The levels of a hierarchical run, ascending. */
  levels: number[];
  summary: Record<string, unknown> | null;
  /** Projection runs: the commit the run read and whether the server's cache answered. */
  commit?: number;
  cached?: boolean;
};

type Row = Record<string, Term | undefined>;

const str = (t: Term | undefined) => (t && t.type !== 'triple' ? t.value : undefined);

/**
 * The level a hierarchical lens shows: the chosen one, or the last the run has, which is the
 * coarsest and the one whose modularity the summary reports.
 */
export function pickLevel(levels: readonly number[], want: number | undefined): number | undefined {
  if (!levels.length) return undefined;
  if (want != null && levels.includes(want)) return want;
  return levels[levels.length - 1];
}

/** The rows of a run as values per node, at `level` for a hierarchical run. */
export function readRows(spec: AlgoSpec, rows: readonly Row[], level?: number): AlgoRows {
  const levels = spec.hierarchical
    ? [...new Set(rows.map((r) => Number(str(r.level))).filter(Number.isFinite))].sort(
        (a, b) => a - b,
      )
    : [];
  const at = pickLevel(levels, level);
  const values = new Map<string, number | string>();
  const sizes = new Map<string, number>();
  let summary: Record<string, unknown> | null = null;
  for (const r of rows) {
    if (summary == null && r.summary) {
      try {
        summary = JSON.parse(str(r.summary) ?? 'null');
      } catch {
        summary = null;
      }
    }
    if (at != null && Number(str(r.level)) !== at) continue;
    const n = str(r.node);
    const v = str(r[spec.output]);
    if (!n || v == null) continue;
    if (spec.channel === 'size') {
      const x = Number(v);
      if (Number.isFinite(x)) values.set(n, x);
    } else {
      values.set(n, v);
      const size = Number(str(r.communitySize));
      if (Number.isFinite(size)) sizes.set(v, size);
    }
  }
  return { values, sizes, levels, summary };
}

/** A run's summary in a few words, such as "20 iterations, converged" or "modularity 0.41". */
export function summaryText(s: Record<string, unknown> | null): string | undefined {
  if (!s) return undefined;
  const parts: string[] = [];
  const num = (k: string) => (typeof s[k] === 'number' ? (s[k] as number) : undefined);
  const it = num('iterations');
  if (it != null) parts.push(`${it} ${it === 1 ? 'iteration' : 'iterations'}`);
  if (typeof s.converged === 'boolean') parts.push(s.converged ? 'converged' : 'did not converge');
  const mod = num('modularity');
  if (mod != null) parts.push(`modularity ${mod.toFixed(2)}`);
  const c = num('communities') ?? num('components');
  if (c != null) parts.push(`${c.toLocaleString('en-US')} in all`);
  return parts.length ? parts.join(', ') : undefined;
}

export type DrawnContext = {
  ds: string;
  at?: string;
  signal?: AbortSignal;
  select?: (ds: string, sparql: string, opts: api.QueryOptions) => Promise<Row[]>;
  limiter?: Limiter;
};

/** Run an algorithm over the drawn graph. */
export async function runDrawn(
  ctx: DrawnContext,
  spec: AlgoSpec,
  iris: readonly string[],
  params: Record<string, number | string>,
  level?: number,
): Promise<AlgoRows> {
  if (iris.length > MAX_DRAWN)
    throw new Error(
      `Drawn-graph runs take at most ${MAX_DRAWN.toLocaleString('en-US')} nodes, and ${iris.length.toLocaleString('en-US')} are drawn. Use projection scope.`,
    );
  const select = ctx.select ?? api.select;
  const done = ctx.limiter ? await ctx.limiter() : () => {};
  try {
    const rows = await select(ctx.ds, drawnQuery(spec, iris, params), {
      ...(ctx.at ? { at: ctx.at } : {}),
      signal: ctx.signal,
    });
    return readRows(spec, rows, level);
  } finally {
    done();
  }
}

/** A projection a run names: a stored one by name, or the predicates of an inline one. */
export type ProjectionRef = { name: string } | { predicates: string[] };

/** The projection scope is not on this server: it predates view-only runs. */
export class NoViewRuns extends Error {}

type RunAnswer = {
  head: { vars: string[] };
  results: { bindings: Row[] };
  summary?: Record<string, unknown> | null;
  run?: { commit: number; cached: boolean; rows: number };
};

/** The answer of `POST /$/algo/{ds}/run`: `202` with a task, or the rows. */
type Posted = { task: api.Task & { joined?: boolean } } | { answer: RunAnswer };

export type ProjectionContext = {
  ds: string;
  at?: string;
  signal?: AbortSignal;
  /** Called with a background run's task as it changes. */
  onTask?: (t: api.Task) => void;
  /** Poll interval of a background run, in ms. */
  poll?: number;
  /** Fetch, for tests. */
  post?: (path: string, body: unknown, signal?: AbortSignal) => Promise<Posted>;
  getTask?: (id: string) => Promise<api.Task>;
};

async function postRun(path: string, body: unknown, signal?: AbortSignal): Promise<Posted> {
  let res: Response;
  try {
    res = await api.request(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/sparql-results+json' },
      body: JSON.stringify(body),
      signal,
    });
  } catch (e) {
    if (
      e instanceof api.ApiError &&
      e.status === 400 &&
      /unknown field `(nodes|keep|async|top)`/.test(e.message)
    )
      throw new NoViewRuns(
        'Projection scope needs a server with view-only algorithm runs, and this one predates them.',
      );
    throw e;
  }
  const j = await res.json();
  return res.status === 202 ? { task: j } : { answer: j };
}

const sleep = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    const t = setTimeout(resolve, ms);
    signal?.addEventListener(
      'abort',
      () => {
        clearTimeout(t);
        reject(signal.reason ?? new DOMException('aborted', 'AbortError'));
      },
      { once: true },
    );
  });

/**
 * Run an algorithm over a projection of the dataset and read the values of `iris`: start a
 * kept background run, follow its task, then read the kept result for the nodes in requests of
 * at most 20,000. With `top`, each request asks for its highest `top` rows and the answer keeps
 * the highest `top` of all.
 */
export async function runProjection(
  ctx: ProjectionContext,
  spec: AlgoSpec,
  projection: ProjectionRef,
  iris: readonly string[],
  params: Record<string, number | string>,
  opts: { level?: number; top?: number } = {},
): Promise<AlgoRows> {
  const post = ctx.post ?? postRun;
  const getTask = ctx.getTask ?? api.getTask;
  const q = new URLSearchParams();
  if (ctx.at) q.set('at', ctx.at);
  const path = `/$/algo/${encodeURIComponent(ctx.ds)}/run${q.size ? `?${q}` : ''}`;
  const base = {
    algorithm: spec.id,
    projection: 'name' in projection ? projection.name : { predicates: projection.predicates },
    params,
    keep: true,
  };
  const chunks = chunk(iris, MAX_RUN_NODES);
  if (!chunks.length) chunks.push([]);
  const ask = (nodes: readonly string[], background: boolean) =>
    post(
      path,
      {
        ...base,
        nodes,
        ...(opts.top ? { top: opts.top } : {}),
        ...(background ? { async: true } : {}),
      },
      ctx.signal,
    );
  let first = await ask(chunks[0], true);
  if ('task' in first) {
    let t: api.Task = first.task;
    ctx.onTask?.(t);
    while (t.state === 'running' || t.state === 'queued') {
      await sleep(ctx.poll ?? 500, ctx.signal);
      t = await getTask(t.id);
      ctx.onTask?.(t);
    }
    if (t.state !== 'done')
      throw new Error(
        t.state === 'cancelled'
          ? 'The run was cancelled.'
          : `The run failed: ${t.message ?? 'no message'}`,
      );
    const kept = (t.detail as { kept?: boolean } | undefined)?.kept;
    if (kept === false)
      throw new Error(
        'The run finished, but the server had no memory to keep its result. Narrow the projection, or try again later.',
      );
    first = await ask(chunks[0], false);
    if ('task' in first) throw new Error('The kept result went away before it could be read.');
  }
  const answers: RunAnswer[] = [first.answer];
  for (const c of chunks.slice(1)) {
    const a = await ask(c, false);
    if ('task' in a) throw new Error('The kept result went away before it could be read.');
    answers.push(a.answer);
  }
  const rows = answers.flatMap((a) => a.results.bindings);
  // the summary is not a row here: give it to the reader as the first row's
  if (rows.length && first.answer.summary)
    rows[0] = {
      ...rows[0],
      summary: { type: 'literal', value: JSON.stringify(first.answer.summary) },
    };
  let out = readRows(spec, rows, opts.level);
  if (!rows.length && first.answer.summary) out = { ...out, summary: first.answer.summary };
  if (opts.top && out.values.size > opts.top) {
    const best = [...out.values].sort((a, b) => Number(b[1]) - Number(a[1])).slice(0, opts.top);
    out = { ...out, values: new Map(best) };
  }
  return { ...out, commit: first.answer.run?.commit, cached: first.answer.run?.cached };
}

/** Stop a background run with `DELETE /$/tasks/{id}`. False when the server refused. */
export async function cancelRun(id: string): Promise<boolean> {
  try {
    await api.request(`/$/tasks/${encodeURIComponent(id)}`, { method: 'DELETE' });
    return true;
  } catch {
    return false;
  }
}

/** The dataset's named projections, for the projection picker. Empty when they cannot be read. */
export async function projections(ds: string, signal?: AbortSignal): Promise<string[]> {
  try {
    const j = await api.json<{ projections?: { name: string }[] }>(
      `/$/algo/${encodeURIComponent(ds)}/projections`,
      { signal },
    );
    return (j?.projections ?? []).map((p) => p.name).sort();
  } catch {
    return [];
  }
}

/** The predicates of the drawn edges, for an inline projection. */
export function drawnPredicates(edges: readonly { triple?: [Term, Term, Term] }[]): string[] {
  const out = new Set<string>();
  for (const e of edges) {
    const p = e.triple?.[1];
    if (p?.type === 'uri' && !p.value.startsWith('urn:var:')) out.add(p.value);
  }
  return [...out].sort();
}

export type Memberships = {
  available: boolean;
  reason?: string;
  memberships: { node: string; community: string; level?: number }[];
  communities: { iri: string; level?: number; memberCount?: number; title?: string | null }[];
};

/** The GraphRAG communities of the nodes at `level` (C22, §2.3), in requests of 20,000. */
export async function readMemberships(
  ds: string,
  iris: readonly string[],
  level: number | undefined,
  signal?: AbortSignal,
): Promise<Memberships> {
  const out: Memberships = { available: true, memberships: [], communities: [] };
  const seen = new Set<string>();
  for (const c of chunk(iris, MAX_RUN_NODES)) {
    const j = await api.json<Memberships>(`/$/graphrag/${encodeURIComponent(ds)}/memberships`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ nodes: c, ...(level != null ? { level } : {}) }),
      signal,
    });
    if (!j.available) return { ...j, memberships: [], communities: [] };
    out.memberships.push(...j.memberships);
    for (const k of j.communities)
      if (!seen.has(k.iri)) {
        seen.add(k.iri);
        out.communities.push(k);
      }
  }
  return out;
}
