// The data of the Phase 1 lenses (docs/specs/U10-canvas-lenses.md §3.1 and §5): degree,
// a property's values, class, graph and inferred edges, read for the drawn elements with one
// query per 200 terms or edges, at most four in flight, at the canvas's state. A cache per
// lens and state keeps what was read, so that a grown graph only reads its new elements.
import * as api from './api';
import type { Term } from './api';
import { candidateQuery, edgeIdOf, type EdgeTriple } from './explore-time';
import { RDF_TYPE, sparqlIri } from './rdf';

/** Terms or edges per query (§5). */
export const BATCH = 200;
/** Queries in flight at once (§5). */
export const IN_FLIGHT = 4;
/** Drawn nodes the lenses apply to at most (§5). */
export const MAX_LENS_NODES = 20_000;
export const INFERRED_GRAPH = 'urn:x-sparkles:inferred';

export type LensNode = { id: string; term?: Term };
export type LensEdge = { id: string; source: string; target: string; triple?: EdgeTriple };

type Row = Record<string, Term | undefined>;
export type SelectFn = (ds: string, sparql: string, opts: api.QueryOptions) => Promise<Row[]>;

/** Where and how the lens queries read: the canvas's dataset (with its branch) and state. */
export type ReadContext = {
  ds: string;
  at?: string;
  signal?: AbortSignal;
  /** The query function, `api.select` unless a test gives another. */
  select?: SelectFn;
};

const ROWS = 100_000;

function run(ctx: ReadContext, sparql: string, extra: api.QueryOptions = {}): Promise<Row[]> {
  const select = ctx.select ?? api.select;
  return select(ctx.ds, sparql, {
    send: ROWS,
    ...(ctx.at ? { at: ctx.at } : {}),
    signal: ctx.signal,
    ...extra,
  });
}

/** `xs` in chunks of `n`. */
export function chunk<T>(xs: readonly T[], n = BATCH): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < xs.length; i += n) out.push(xs.slice(i, i + n));
  return out;
}

/** `fn` over `items` with at most `limit` calls in flight, results in order. */
export async function pool<T, R>(
  items: readonly T[],
  fn: (t: T) => Promise<R>,
  limit = IN_FLIGHT,
): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return out;
}

/** The value of a term as a string, undefined for a triple term. */
const str = (t: Term | undefined) => (t && t.type !== 'triple' ? t.value : undefined);

/** The IRI nodes of `nodes`, the only ones a `VALUES` block of the lens queries can name. */
export function iriNodes(nodes: readonly LensNode[]): { id: string; iri: string }[] {
  return nodes.flatMap((n) => (n.term?.type === 'uri' ? [{ id: n.id, iri: n.term.value }] : []));
}

const values = (iris: readonly string[]) => `VALUES ?n { ${iris.map(sparqlIri).join(' ')} }`;

/** The triples each node takes part in, as subject or object (§3.1, one grouped count). */
export function degreeQuery(iris: readonly string[]): string {
  return `SELECT ?n (COUNT(*) AS ?c) WHERE { ${values(iris)} { ?n ?p ?o } UNION { ?s ?p ?n } } GROUP BY ?n`;
}

/** The values of one property of each node. */
export function propertyQuery(iris: readonly string[], property: string): string {
  return `SELECT ?n ?v WHERE { ${values(iris)} ?n ${sparqlIri(property)} ?v }`;
}

/** The classes of each node, read without reasoning so that they are the asserted ones. */
export function classQuery(iris: readonly string[]): string {
  return `SELECT ?n ?v WHERE { ${values(iris)} ?n ${sparqlIri(RDF_TYPE)} ?v FILTER(isIRI(?v)) }`;
}

/** The named graphs each node is used in, as subject or object, the inferred graph aside. */
export function graphQuery(iris: readonly string[]): string {
  return `SELECT DISTINCT ?n ?v WHERE { ${values(iris)} GRAPH ?v { { ?n ?p ?o } UNION { ?s ?p ?n } } FILTER(?v != ${sparqlIri(INFERRED_GRAPH)}) }`;
}

/** The properties of the nodes, most used first, with how many of their values are numbers. */
export function propertiesQuery(iris: readonly string[]): string {
  return `SELECT ?p (COUNT(*) AS ?c) (SUM(IF(isNumeric(?o), 1, 0)) AS ?num) WHERE { ${values(iris)} ?n ?p ?o } GROUP BY ?p ORDER BY DESC(?c) LIMIT 200`;
}

/** Cached values per node id. A node that was read and has nothing holds an empty list. */
export type ValueCache = Map<string, Term[]>;

/**
 * The values `query` reads for each IRI node, read only for the nodes the cache lacks, in
 * batches of `BATCH` with at most `IN_FLIGHT` queries at once. Rows bind `?n` and `?v`.
 */
export async function readValues(
  ctx: ReadContext,
  nodes: readonly LensNode[],
  query: (iris: string[]) => string,
  cache: ValueCache,
  extra: api.QueryOptions = {},
): Promise<ValueCache> {
  const missing = iriNodes(nodes).filter((n) => !cache.has(n.id));
  const byIri = new Map<string, string[]>();
  for (const n of missing) byIri.set(n.iri, [...(byIri.get(n.iri) ?? []), n.id]);
  const iris = [...byIri.keys()];
  const answers = await pool(chunk(iris), (b) => run(ctx, query(b), extra));
  const got = new Map<string, Term[]>();
  for (const rows of answers)
    for (const r of rows) {
      const n = str(r.n);
      if (n && r.v) got.set(n, [...(got.get(n) ?? []), r.v]);
    }
  for (const [iri, ids] of byIri) for (const id of ids) cache.set(id, got.get(iri) ?? []);
  return cache;
}

/** The degree of each IRI node in the dataset, read for the nodes the cache lacks. */
export async function readDegrees(
  ctx: ReadContext,
  nodes: readonly LensNode[],
  cache: ValueCache,
): Promise<Map<string, number | null>> {
  const missing = iriNodes(nodes).filter((n) => !cache.has(n.id));
  const answers = await pool(chunk([...new Set(missing.map((n) => n.iri))]), (b) =>
    run(ctx, degreeQuery(b)),
  );
  const got = new Map<string, Term>();
  for (const rows of answers) for (const r of rows) if (str(r.n) && r.c) got.set(str(r.n)!, r.c);
  for (const n of missing) {
    const c = got.get(n.iri);
    cache.set(n.id, [c ?? { type: 'literal', value: '0' }]);
  }
  const out = new Map<string, number | null>();
  for (const n of nodes) {
    const v = cache.get(n.id)?.[0];
    out.set(n.id, v ? Number(str(v)) : null);
  }
  return out;
}

/** The degree of each node in the drawn graph, counted in the browser (§3.1). */
export function drawnDegrees(
  nodes: readonly LensNode[],
  edges: readonly LensEdge[],
): Map<string, number> {
  const out = new Map(nodes.map((n) => [n.id, 0]));
  for (const e of edges) {
    if (out.has(e.source)) out.set(e.source, out.get(e.source)! + 1);
    if (e.target !== e.source && out.has(e.target)) out.set(e.target, out.get(e.target)! + 1);
  }
  return out;
}

const NUMERIC = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;

/** A term as a number: a literal whose lexical form is one, otherwise null. */
export function numberOf(t: Term): number | null {
  if (t.type !== 'literal') return null;
  const s = t.value.trim();
  return NUMERIC.test(s) ? Number(s) : null;
}

/** The largest numeric value of each node, null for a node with none. */
export function pickNumber(
  nodes: readonly LensNode[],
  cache: ValueCache,
): Map<string, number | null> {
  const out = new Map<string, number | null>();
  for (const n of nodes) {
    let best: number | null = null;
    for (const t of cache.get(n.id) ?? []) {
      const v = numberOf(t);
      if (v != null && (best == null || v > best)) best = v;
    }
    out.set(n.id, best);
  }
  return out;
}

/**
 * One category per node. A node with several values takes the one most drawn nodes share,
 * so that the categories gather, and between equals the smallest in code-point order. A node
 * without values is null.
 */
export function pickCommon(
  nodes: readonly LensNode[],
  lists: Map<string, string[]>,
): Map<string, string | null> {
  const freq = new Map<string, number>();
  for (const n of nodes)
    for (const v of new Set(lists.get(n.id) ?? [])) freq.set(v, (freq.get(v) ?? 0) + 1);
  const out = new Map<string, string | null>();
  for (const n of nodes) {
    const vs = [...new Set(lists.get(n.id) ?? [])];
    vs.sort((a, b) => freq.get(b)! - freq.get(a)! || (a < b ? -1 : a > b ? 1 : 0));
    out.set(n.id, vs[0] ?? null);
  }
  return out;
}

/** The category key of a term: an IRI, or a literal's lexical form. */
export const categoryOf = (t: Term): string | undefined =>
  t.type === 'uri' ? t.value : t.type === 'literal' ? t.value : undefined;

/** The node used in several named graphs. */
export const SEVERAL_GRAPHS = '\u0000several';
/** The node used in no named graph, only in the default graph. */
export const DEFAULT_GRAPH = '\u0000default';

/**
 * The graph category of each node: its named graph, `SEVERAL_GRAPHS` when it is used in more
 * than one, `DEFAULT_GRAPH` for an IRI used in none, and null for a node a read cannot name.
 */
export function pickGraph(
  nodes: readonly LensNode[],
  cache: ValueCache,
): Map<string, string | null> {
  const out = new Map<string, string | null>();
  for (const n of nodes) {
    const gs = cache.get(n.id);
    if (!gs) {
      out.set(n.id, null);
      continue;
    }
    const names = [...new Set(gs.map(categoryOf).filter((g): g is string => !!g))];
    out.set(n.id, names.length > 1 ? SEVERAL_GRAPHS : (names[0] ?? DEFAULT_GRAPH));
  }
  return out;
}

export type PropertyInfo = { iri: string; count: number; numeric: boolean };

/** The properties of the drawn nodes, for the property lens's picker. */
export async function readProperties(
  ctx: ReadContext,
  nodes: readonly LensNode[],
): Promise<PropertyInfo[]> {
  const iris = [...new Set(iriNodes(nodes).map((n) => n.iri))].slice(0, BATCH);
  if (!iris.length) return [];
  const rows = await run(ctx, propertiesQuery(iris));
  return rows
    .filter((r) => str(r.p))
    .map((r) => {
      const count = Number(str(r.c) ?? 0);
      return { iri: str(r.p)!, count, numeric: Number(str(r.num) ?? 0) * 2 >= count };
    });
}

/** Whether a read can name the triple of an edge: no blank node, triple term or variable. */
export function checkable(t: EdgeTriple | undefined): t is EdgeTriple {
  return (
    !!t &&
    t.every((x) => x.type === 'uri' || x.type === 'literal') &&
    t[0].type === 'uri' &&
    t[1].type === 'uri' &&
    !t[1].value.startsWith('urn:var:')
  );
}

/**
 * The drawn edges the reasoner added (§2.2, §3.1): visible with reasoning and absent without
 * it, which covers materialized inferences and RDFS on read alike. The edges are read with
 * `reasoning=false` in `VALUES ?s ?p ?o` batches of 200. With `verify`, they are also read
 * with reasoning, for edges that do not come from a read of the data, such as those of a
 * CONSTRUCT result, which are otherwise neither. `cache` holds each checked edge's answer,
 * true for inferred, by edge id.
 */
export async function readInferred(
  ctx: ReadContext,
  edges: readonly LensEdge[],
  cache: Map<string, boolean>,
  verify = false,
): Promise<{ inferred: Set<string>; unchecked: Set<string> }> {
  const unchecked = new Set<string>();
  const todo: { id: string; triple: EdgeTriple }[] = [];
  for (const e of edges) {
    if (!checkable(e.triple)) unchecked.add(e.id);
    else if (!cache.has(e.id)) todo.push({ id: e.id, triple: e.triple });
  }
  const keys = [...new Set(todo.map((t) => edgeIdOf(t.triple)))];
  const byKey = new Map<string, EdgeTriple>();
  for (const t of todo) byKey.set(edgeIdOf(t.triple), t.triple);
  const batches = chunk(keys).map((b) => candidateQuery(b.map((k) => byKey.get(k)!)));
  const found = async (reasoning: boolean | undefined) => {
    const out = new Set<string>();
    const answers = await pool(batches, (q) =>
      q ? run(ctx, q, reasoning == null ? {} : { reasoning }) : Promise.resolve([]),
    );
    for (const rows of answers)
      for (const r of rows) if (r.s && r.p && r.o) out.add(edgeIdOf([r.s, r.p, r.o]));
    return out;
  };
  const [without, withIt] = await Promise.all([
    found(false),
    verify ? found(undefined) : Promise.resolve(null),
  ]);
  for (const t of todo) {
    const k = edgeIdOf(t.triple);
    cache.set(t.id, !without.has(k) && (withIt ? withIt.has(k) : true));
  }
  const inferred = new Set<string>();
  for (const e of edges) if (cache.get(e.id) && !unchecked.has(e.id)) inferred.add(e.id);
  return { inferred, unchecked };
}

/** Inferred edges per node, for the "has inferred edges" badge. */
export function inferredPerNode(
  edges: readonly LensEdge[],
  inferred: ReadonlySet<string>,
): Map<string, number> {
  const out = new Map<string, number>();
  for (const e of edges) {
    if (!inferred.has(e.id)) continue;
    out.set(e.source, (out.get(e.source) ?? 0) + 1);
    if (e.target !== e.source) out.set(e.target, (out.get(e.target) ?? 0) + 1);
  }
  return out;
}
