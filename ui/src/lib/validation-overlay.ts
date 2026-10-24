// The validation overlay of Explore (docs/specs/U07-data-quality-and-schema-diagram.md §11):
// SHACL and ShEx results for the drawn nodes, read through the validation routes' focus
// sets and stored shapes, normalized to one result type, and turned into badges.
import {
  ApiError,
  request,
  type DiffQuad,
  type GuardReport,
  type ShaclReport,
  type ShaclResult,
  type ShexFailure,
  type ShexReport,
  type ShexResult,
  type Term,
} from './api';
import { fmtInt } from './format';
import { termKey, WELL_KNOWN, type PrefixMap } from './rdf';

const enc = encodeURIComponent;

export type Severity = 'violation' | 'warning' | 'info';

/** One result of either language, in the form the overlay draws and suggests fixes for. */
export type VResult = {
  lang: 'shacl' | 'shex';
  focus: Term;
  /** The source shape: an IRI, a blank node, or `START` as a URI-less label. */
  shape: string;
  /** The constraint: a SHACL component's local name without `ConstraintComponent`, in
   * lower camel case (`minCount`), or a ShEx failure kind (`cardinality`). */
  kind: string;
  /** The full component IRI of a SHACL result. */
  component?: string;
  /** The predicate of a path that is a single predicate (or a ShEx arc), else null. */
  path: string | null;
  /** A ShEx arc followed backwards. */
  inverse?: boolean;
  /** The path as text when it is more than one predicate. */
  pathText: string | null;
  value: Term | null;
  severity: Severity;
  messages: string[];
  /** A ShEx failure's constraint text, such as `xsd:integer` or a value set. */
  constraint?: string;
  /** A ShEx cardinality's bounds and count. */
  min?: number;
  max?: number | null;
  count?: number;
};

const SH = WELL_KNOWN.sh;

function severityOf(t: Term | undefined): Severity {
  const v = t?.type === 'uri' ? t.value : '';
  if (v === SH + 'Warning') return 'warning';
  if (v === SH + 'Info') return 'info';
  return 'violation';
}

/** The short name of a SHACL constraint component: `sh:MinCountConstraintComponent` is `minCount`. */
export function componentKind(iri: string): string {
  const local = iri.startsWith(SH) ? iri.slice(SH.length) : iri.replace(/^.*[#/]/, '');
  const base = local.replace(/ConstraintComponent$/, '');
  return base.charAt(0).toLowerCase() + base.slice(1);
}

const shapeText = (t: Term | { type: 'start' } | undefined): string =>
  !t || t.type === 'start'
    ? 'START'
    : t.type === 'bnode'
      ? `_:${t.value}`
      : typeof t.value === 'string'
        ? t.value
        : '';

/** A SHACL result in the overlay's form. */
export function fromShacl(r: ShaclResult): VResult {
  const p = r.resultPath;
  const component =
    r.sourceConstraintComponent?.type === 'uri' ? r.sourceConstraintComponent.value : '';
  return {
    lang: 'shacl',
    focus: r.focusNode,
    shape: shapeText(r.sourceShape),
    kind: componentKind(component),
    component,
    path: p && p.type === 'uri' ? p.value : null,
    pathText: p && p.type === 'path' ? String(p.value) : null,
    value: r.value ?? null,
    severity: severityOf(r.severity),
    messages: r.messages ?? [],
  };
}

/** The results of a nonconformant ShEx association, one per failure (each a violation). */
export function fromShex(r: ShexResult): VResult[] {
  if (r.status !== 'nonconformant') return [];
  const base = {
    lang: 'shex' as const,
    focus: r.node,
    shape: shapeText(r.shape),
    severity: 'violation' as const,
    path: null,
    pathText: null,
    value: null,
    messages: r.reason ? [r.reason] : [],
  };
  const failures = r.appinfo?.failures ?? [];
  if (!failures.length) return [{ ...base, kind: 'noMatch' }];
  return failures.map((f: ShexFailure): VResult => {
    switch (f.kind) {
      case 'cardinality':
        return {
          ...base,
          kind: f.kind,
          path: f.predicate,
          inverse: f.inverse,
          min: f.min,
          max: f.max,
          count: f.count,
        };
      case 'closed':
      case 'extra':
        return { ...base, kind: f.kind, path: f.predicate, value: f.value };
      case 'nodeKind':
      case 'datatype':
      case 'facet':
      case 'valueSet':
        return { ...base, kind: f.kind, value: f.value, constraint: f.constraint };
      case 'reference':
        return { ...base, kind: f.kind, value: f.value, constraint: f.shape };
      case 'noMatch':
        return { ...base, kind: f.kind, messages: [f.detail] };
      case 'semAct':
        return { ...base, kind: f.kind, messages: [f.message] };
      default:
        return { ...base, kind: f.kind };
    }
  });
}

// --- where the overlay's shapes come from --------------------------------------------

/** The overlay's source: the write-time guard, a shapes graph of the dataset, or the
 * Validate panel's SHACL shapes. */
export type OverlaySource =
  | { kind: 'guard' }
  | { kind: 'graph'; iri: string }
  | { kind: 'editor'; shapes: string; syntax: 'turtle' | 'shaclc' };

/** What the dataset offers the caller: a guard of a language, no guard, or no overlay. */
export type GuardProbe =
  | { state: 'shacl' | 'shex' | 'none' }
  | { state: 'restricted' | 'unavailable'; message: string };

/** The reason shown when the caller's grants cover part of the dataset (§11.2). */
export const RESTRICTED_TEXT = 'Validation needs access to every graph';

const PROBE_NODE = '<urn:x-sparkles:overlay-probe>';

/**
 * Find the guard's language with one cheap request: a SHACL validation of a node that
 * is not in the data against `shapes=guard`. A ShEx guard answers 400 naming ShEx, no
 * guard 404, a caller limited to some graphs 403.
 */
export async function probeGuard(ds: string, signal?: AbortSignal): Promise<GuardProbe> {
  try {
    await request(`/${enc(ds)}/shacl?shapes=guard`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded', Accept: 'application/json' },
      body: new URLSearchParams({ target: PROBE_NODE }).toString(),
      signal,
    });
    return { state: 'shacl' };
  } catch (e) {
    if (!(e instanceof ApiError)) throw e;
    if (e.status === 404) return { state: 'none' };
    if (e.status === 400 && /validates ShEx/.test(e.message)) return { state: 'shex' };
    if (e.status === 403)
      return {
        state: 'restricted',
        message: /limited to some graphs/.test(e.message) ? RESTRICTED_TEXT : e.message,
      };
    if (e.status === 501) return { state: 'unavailable', message: e.message };
    throw e;
  }
}

/** Requests of the overlay: at most this many nodes each, one in flight (§11.6). */
export const BATCH = 500;
/** With the editor's shapes the nodes go in the query string, so batches are smaller. */
export const EDITOR_BATCH = 100;
/** The most drawn nodes the overlay validates (§11.6). */
export const MAX_DRAWN = 1500;

/** A node as the routes take it: an IRI in angle brackets or a blank node label. */
const nodeArg = (t: Term) => (t.type === 'uri' ? `<${t.value}>` : `_:${t.value}`);

/** The nodes the overlay can validate: IRIs and blank nodes, first `MAX_DRAWN`. */
export function focusable(terms: Iterable<Term>): Term[] {
  const out: Term[] = [];
  const seen = new Set<string>();
  for (const t of terms) {
    if (t.type !== 'uri' && t.type !== 'bnode') continue;
    const k = termKey(t);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(t);
    if (out.length >= MAX_DRAWN) break;
  }
  return out;
}

function chunks<T>(xs: T[], n: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < xs.length; i += n) out.push(xs.slice(i, i + n));
  return out;
}

export type ValidateOpts = {
  /** The state to validate at (`at=`); the head when empty. */
  at?: string;
  signal?: AbortSignal;
  /** Called after each batch with the results so far. */
  onbatch?: (results: VResult[], done: number, total: number) => void;
};

/** Validate one batch of nodes against a source. */
async function batch(
  ds: string,
  source: OverlaySource,
  lang: 'shacl' | 'shex',
  nodes: Term[],
  opts: ValidateOpts,
): Promise<VResult[]> {
  const q = new URLSearchParams();
  if (opts.at) q.set('at', opts.at);
  if (lang === 'shex') {
    q.set('schema', 'guard');
    q.set('results', 'nonconformant');
    const res = await request(`/${enc(ds)}/shex?${q}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded', Accept: 'application/json' },
      body: new URLSearchParams({
        map: nodes.map((n) => `${nodeArg(n)}@START`).join(','),
      }).toString(),
      signal: opts.signal,
    });
    const report = (await res.json()) as ShexReport;
    return report.results.flatMap(fromShex);
  }
  let res: Response;
  if (source.kind === 'editor') {
    for (const n of nodes) q.append('target', nodeArg(n));
    res = await request(`/${enc(ds)}/shacl?${q}`, {
      method: 'POST',
      headers: {
        'Content-Type': source.syntax === 'shaclc' ? 'text/shaclc' : 'text/turtle',
        Accept: 'application/json',
      },
      body: source.shapes,
      signal: opts.signal,
    });
  } else {
    q.set('shapes', source.kind === 'guard' ? 'guard' : source.iri);
    const form = new URLSearchParams();
    for (const n of nodes) form.append('target', nodeArg(n));
    res = await request(`/${enc(ds)}/shacl?${q}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded', Accept: 'application/json' },
      body: form.toString(),
      signal: opts.signal,
    });
  }
  const report = (await res.json()) as ShaclReport;
  return report.results.map(fromShacl);
}

/**
 * Validate `nodes` against `source`, in batches of `BATCH` (`EDITOR_BATCH` for the
 * editor's shapes), one in flight. A ShEx guard is `lang: 'shex'`.
 */
export async function validateNodes(
  ds: string,
  source: OverlaySource,
  lang: 'shacl' | 'shex',
  nodes: Term[],
  opts: ValidateOpts = {},
): Promise<VResult[]> {
  const size = source.kind === 'editor' ? EDITOR_BATCH : BATCH;
  const all: VResult[] = [];
  const parts = chunks(nodes, size);
  let done = 0;
  for (const part of parts) {
    all.push(...(await batch(ds, source, lang, part, opts)));
    done += part.length;
    opts.onbatch?.(all, done, nodes.length);
  }
  return all;
}

// --- badges ------------------------------------------------------------------------

export type Badge = {
  count: number;
  worst: Severity;
  /** The mark and the count, such as `✕2`. */
  text: string;
  /** The counts in words, such as `2 violations` or `1 violation, 1 warning`. */
  words: string;
};

export const MARK: Record<Severity, string> = { violation: '✕', warning: '⚠', info: 'ⓘ' };
const RANK: Record<Severity, number> = { violation: 3, warning: 2, info: 1 };

const plural = (n: number, w: string) => `${fmtInt(n)} ${w}${n === 1 ? '' : 's'}`;

/** The counts of some results by severity, in words, worst first. */
export function countsText(rs: { severity: Severity }[]): string {
  const c = { violation: 0, warning: 0, info: 0 };
  for (const r of rs) c[r.severity]++;
  const parts: string[] = [];
  if (c.violation) parts.push(plural(c.violation, 'violation'));
  if (c.warning) parts.push(plural(c.warning, 'warning'));
  if (c.info) parts.push(`${fmtInt(c.info)} info`);
  return parts.join(', ');
}

/** The id Explore draws a literal value node with: one node per subject, predicate and value. */
export const literalNodeId = (focus: Term, predicate: string, value: Term) =>
  `lit:${termKey(focus)}|${predicate}|${termKey(value)}`;

/**
 * The drawn nodes a result belongs to: its focus node, and a literal value drawn as its
 * own node. When the path is not one predicate, a literal node of the focus with the
 * same value counts.
 */
export function nodesOf(r: VResult, drawn: ReadonlySet<string>): string[] {
  const out: string[] = [];
  const focus = termKey(r.focus);
  if (drawn.has(focus)) out.push(focus);
  if (r.value?.type === 'literal') {
    if (r.path && !r.inverse) {
      const id = literalNodeId(r.focus, r.path, r.value);
      if (drawn.has(id)) out.push(id);
    } else {
      const head = `lit:${focus}|`;
      const tail = `|${termKey(r.value)}`;
      for (const id of drawn) if (id.startsWith(head) && id.endsWith(tail)) out.push(id);
    }
  }
  return out;
}

/** The results of each drawn node. */
export function resultsByNode(
  results: VResult[],
  drawn: ReadonlySet<string>,
): Map<string, VResult[]> {
  const m = new Map<string, VResult[]>();
  for (const r of results) {
    for (const id of nodesOf(r, drawn)) {
      const l = m.get(id);
      if (l) l.push(r);
      else m.set(id, [r]);
    }
  }
  return m;
}

/** A badge per drawn node with results: its count and its worst severity. */
export function badges(byNode: Map<string, VResult[]>): Map<string, Badge> {
  const out = new Map<string, Badge>();
  for (const [id, rs] of byNode) {
    let worst: Severity = 'info';
    for (const r of rs) if (RANK[r.severity] > RANK[worst]) worst = r.severity;
    out.set(id, {
      count: rs.length,
      worst,
      text: `${MARK[worst]}${rs.length}`,
      words: countsText(rs),
    });
  }
  return out;
}

/** The text of the list's Validation column: the counts, or nothing for a clean node. */
export const validationCell = (b: Badge | undefined) => (b ? b.words : '');

/** A node's results grouped by shape and constraint, in first-seen order. */
export function groupResults(rs: VResult[]): { shape: string; kind: string; results: VResult[] }[] {
  const groups = new Map<string, { shape: string; kind: string; results: VResult[] }>();
  for (const r of rs) {
    const k = `${r.shape}\u0000${r.kind}`;
    const g = groups.get(k) ?? { shape: r.shape, kind: r.kind, results: [] };
    g.results.push(r);
    groups.set(k, g);
  }
  return [...groups.values()];
}

/** The results panel lists this many results of a node and counts the rest (§11.6). */
export const PANEL_LIMIT = 100;

// --- previews of a suggested fix (C15) ---------------------------------------------

/** The preview document of a dry run of `/{ds}/update` (docs/specs/C15-write-previews.md §2.3). */
export type WritePreview = {
  dryRun: true;
  wouldCommit: boolean;
  outcome: string;
  head: number;
  commit?: { inserted: number; deleted: number };
  changes?: { total: number; limit: number; truncated: boolean; quads: DiffQuad[] };
  validation?: GuardReport & { strategy?: string };
  error?: string;
};

/**
 * A dry run of an update (`dryRun=true`), listing up to `changes` changed quads. A dry
 * run the guard would reject answers 422 with the preview as its body, which is returned
 * like any other preview.
 */
export async function previewUpdate(
  ds: string,
  sparql: string,
  changes = 20,
  signal?: AbortSignal,
): Promise<WritePreview> {
  try {
    const res = await request(`/${enc(ds)}/update?dryRun=true&changes=${changes}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/sparql-update', Accept: 'application/json' },
      body: sparql,
      signal,
    });
    return (await res.json()) as WritePreview;
  } catch (e) {
    if (e instanceof ApiError && e.body && (e.body as { dryRun?: unknown }).dryRun === true)
      return e.body as unknown as WritePreview;
    throw e;
  }
}

const valueOf = (t: unknown): string => {
  const x = (t ?? {}) as { type?: string; value?: unknown };
  if (x.type === 'start') return 'START';
  if (x.type === 'bnode') return `_:${String(x.value ?? '')}`;
  return typeof x.value === 'string' ? x.value : '';
};

/** The statuses of a guard summary that validated the candidate state. */
const CHECKED = new Set(['passed', 'warned', 'rejected']);

/**
 * Whether a preview's guard summary says the result would be resolved. It `remains` when
 * the summary lists it: the same focus node, shape, component, path and value. Its absence
 * means `resolved` only when the summary is conclusive: the guard validated the state
 * (`passed`, `warned` or `rejected`), the list is not truncated, and it lists every result
 * of the state. A full validation lists them all. An incremental one lists only the shapes
 * and focus nodes it validated again, so its list is complete only when its exact `total`
 * equals the number listed. Everything else is `unknown`.
 */
export function resolution(r: VResult, p: WritePreview): 'resolved' | 'remains' | 'unknown' {
  const v = p.validation;
  if (!v || (v.language && v.language !== r.lang)) return 'unknown';
  const focus = valueOf(r.focus);
  const value = r.value ? termKey(r.value) : null;
  const listed = (v.results ?? []).some((x) => {
    if (r.lang === 'shex') return valueOf(x.node) === focus && valueOf(x.shape) === r.shape;
    if (valueOf(x.focusNode) !== focus || valueOf(x.sourceShape) !== r.shape) return false;
    if (r.component && valueOf(x.sourceConstraintComponent) !== r.component) return false;
    if (value != null && (!x.value || termKey(x.value as Term) !== value)) return false;
    const path = r.path ?? r.pathText;
    return path == null || valueOf(x.resultPath) === path;
  });
  if (listed) return 'remains';
  if (v.truncated || !CHECKED.has(String(v.status ?? ''))) return 'unknown';
  if (v.strategy === 'full') return 'resolved';
  return v.total === (v.results ?? []).length ? 'resolved' : 'unknown';
}

/**
 * The graph a suggested fix writes, from the guard's `dataGraph` (see `FixGraph` in
 * fixes.ts): the default graph for none, `default` or a list of only the default graph,
 * the IRI of a list of one named graph, and `choose` for `union` or several graphs. On a
 * server that serves the union as the default graph (`unionDefault`), the default graph
 * is the union, so it also gives `choose`.
 */
export function fixGraph(
  dataGraph: string | string[] | undefined,
  unionDefault = false,
): string | null | 'choose' {
  const DEFAULT = 'urn:x-arq:DefaultGraph';
  const dflt = unionDefault ? 'choose' : null;
  if (dataGraph == null || dataGraph === 'default') return dflt;
  if (typeof dataGraph === 'string') return dataGraph === 'union' ? 'choose' : dflt;
  const named = dataGraph.filter((g) => g !== DEFAULT && g !== 'default');
  if (named.length === 0) return dflt;
  if (unionDefault && named.length < dataGraph.length) return 'choose';
  if (named.length === 1 && named.length === dataGraph.length) return named[0];
  return 'choose';
}

/** The datatype IRI of a ShEx constraint such as `xsd:integer` or `<http://…>`, or null. */
export function expandIri(text: string, prefixes: PrefixMap): string | null {
  const t = text.trim();
  if (t.startsWith('<') && t.endsWith('>')) return t.slice(1, -1);
  const m = /^([A-Za-z][\w.-]*|):(\S*)$/.exec(t);
  if (!m) return null;
  const ns = prefixes[m[1]] ?? WELL_KNOWN[m[1]];
  return ns ? ns + m[2] : null;
}
