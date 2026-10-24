// Facet histograms and their brushes (docs/specs/U11-linked-views.md, sections 2.3 and
// 3.2): the requests of `POST /$/graph/{ds}/facets`, the brush a person drags or clicks,
// the filter text that **Apply to query** writes, which is the text the route applies,
// and the test of a drawn entity's values against the brushes, which dims the canvas.

import { ChangeSet } from '@codemirror/state';
import type {
  FacetAnswer,
  FacetFilter,
  FacetKind,
  FacetSetSpec,
  FacetSpec,
  FacetsRequest,
  Term,
} from './api';
import { applyChanges, compact, EditError, type Change, type PatternOk } from './pattern';
import { agreesWith, editAny, patternVars, type AnyEdit, type Parse } from './pattern-edits';
import { localName, toSparql } from './rdf';

/** The most facets a request counts, as the route allows. */
export const MAX_FACETS = 6;
/** The facets a panel opens with. */
export const FIRST_FACETS = 3;
/** The pause after a brush change before the facets are counted again. */
export const BRUSH_PAUSE_MS = 200;
/** The most entities a `VALUES` block of **Use in query** holds. */
export const VALUES_CAP = 1_000;

const XSD = 'http://www.w3.org/2001/XMLSchema#';

/** A brush on one facet: a range `[from, to)` of a numeric or date facet, or values. */
export type Brush =
  | { property: string; kind: 'numeric' | 'date'; range: [number, number] }
  | { property: string; kind: 'values'; values: Term[] };

/** The bins a histogram asks for: about twice the square root of the set, 8 to 40. */
export function binsFor(size: number): number {
  return Math.min(40, Math.max(8, Math.round(2 * Math.sqrt(Math.max(size, 0)))));
}

/**
 * A number as SPARQL text: the shortest form that reads back as the same number, as the
 * route writes it. A very large or small number may come out with an exponent here and
 * without one there, which SPARQL reads as the same value.
 */
export function numberText(x: number): string {
  return String(x);
}

/** The condition a brush puts on its variable, the same text as the route's. */
export function brushCondition(b: Brush, v: string): string {
  if (b.kind === 'values') return `${v} IN (${b.values.map(toSparql).join(', ')})`;
  const [a, z] = b.range;
  if (b.kind === 'date') return `YEAR(${v}) >= ${numberText(a)} && YEAR(${v}) < ${numberText(z)}`;
  return `${v} >= ${numberText(a)} && ${v} < ${numberText(z)}`;
}

/** A brush as the route's `filters` member. */
export function brushFilter(b: Brush): FacetFilter {
  return b.kind === 'values'
    ? { property: b.property, values: b.values }
    : { property: b.property, range: b.range, kind: b.kind };
}

/** The edges of a range facet's bins. */
export function edgesOf(f: FacetAnswer): number[] {
  const bins = f.bins ?? [];
  if (!bins.length) return [];
  return [...bins.map((b) => b.from), bins[bins.length - 1].to];
}

/** What an answer's facet hands back to the next request, so its bins stay. */
export function frameOf(f: FacetAnswer): FacetSpec {
  if (f.error || !f.kind) return { property: f.property };
  if (f.kind === 'values')
    return { property: f.property, kind: 'values', bars: (f.values ?? []).map((v) => v.value) };
  const edges = edgesOf(f);
  return edges.length >= 2
    ? { property: f.property, kind: f.kind, edges }
    : { property: f.property, kind: f.kind };
}

/** The request for a set, its facets (with the frames already known) and brushes. */
export function facetsRequest(
  within: FacetSetSpec,
  facets: FacetSpec[],
  brushes: Brush[],
  opts: { candidates?: boolean; at?: string } = {},
): FacetsRequest {
  const req: FacetsRequest = { within, facets: facets.slice(0, MAX_FACETS) };
  if (brushes.length) req.filters = brushes.map(brushFilter);
  if (opts.candidates) req.candidates = true;
  if (opts.at) req.at = opts.at;
  return req;
}

/** The brush over bins `i` to `j` of a range facet, in either order. */
export function binsBrush(f: FacetAnswer, i: number, j: number): Brush | null {
  const bins = f.bins ?? [];
  const [a, z] = i <= j ? [i, j] : [j, i];
  if (!bins[a] || !bins[z] || (f.kind !== 'numeric' && f.kind !== 'date')) return null;
  return { property: f.property, kind: f.kind, range: [bins[a].from, bins[z].to] };
}

/** The bins a range brush covers, first and last, or null. */
export function brushedBins(f: FacetAnswer, b: Brush | undefined): [number, number] | null {
  if (!b || b.kind === 'values') return null;
  const bins = f.bins ?? [];
  let first = -1;
  let last = -1;
  bins.forEach((bin, i) => {
    if (bin.from >= b.range[0] && bin.to <= b.range[1]) {
      if (first < 0) first = i;
      last = i;
    }
  });
  return first < 0 ? null : [first, last];
}

/** A bin's label: `30–44` for whole numbers, `0.2–0.4` otherwise, and years for dates. */
export function binLabel(f: FacetAnswer, i: number): string {
  const b = f.bins?.[i];
  if (!b) return '';
  const whole = Number.isInteger(b.from) && Number.isInteger(b.to);
  const to = whole ? b.to - 1 : b.to;
  if (whole && to === b.from) return fmt(b.from, f.kind === 'date');
  return `${fmt(b.from, f.kind === 'date')}–${fmt(to, f.kind === 'date')}`;
}

function fmt(x: number, year: boolean): string {
  return year ? String(x) : x.toLocaleString('en-US', { maximumFractionDigits: 6 });
}

/** How a brush reads aloud and in the count line: "age from 30 to 45". */
export function brushText(b: Brush, name: string, label?: (t: Term) => string): string {
  if (b.kind === 'values') {
    const vals = b.values.map((t) => (label ? label(t) : t.value));
    return `${name} is ${vals.length > 3 ? `${vals.slice(0, 3).join(', ')} and ${vals.length - 3} more` : vals.join(' or ')}`;
  }
  const [a, z] = b.range;
  const whole = Number.isInteger(a) && Number.isInteger(z);
  return whole
    ? `${name} from ${fmt(a, b.kind === 'date')} to ${fmt(z - 1, b.kind === 'date')}`
    : `${name} from ${fmt(a, false)} to below ${fmt(z, false)}`;
}

/** Toggle a value in a bar list's brush; the brush goes when its last value does. */
export function toggleValue(b: Brush | undefined, property: string, value: Term): Brush | null {
  const key = toSparql(value);
  const vals = b && b.kind === 'values' ? b.values : [];
  const next = vals.some((v) => toSparql(v) === key)
    ? vals.filter((v) => toSparql(v) !== key)
    : [...vals, value];
  return next.length ? { property, kind: 'values', values: next } : null;
}

// ------------------------------------------------------------- the drawn entities ----

const NUMERIC = new Set(
  [
    'integer',
    'decimal',
    'double',
    'float',
    'int',
    'long',
    'short',
    'byte',
    'nonNegativeInteger',
    'positiveInteger',
    'negativeInteger',
    'nonPositiveInteger',
    'unsignedInt',
    'unsignedLong',
    'unsignedShort',
    'unsignedByte',
  ].map((l) => XSD + l),
);

/** The number of a numeric literal, or null. */
export function numberOf(t: Term): number | null {
  if (t.type !== 'literal' || !t.datatype || !NUMERIC.has(t.datatype)) return null;
  const s = t.value.trim();
  const x = s === 'INF' ? Infinity : s === '-INF' ? -Infinity : Number(s);
  return Number.isNaN(x) ? null : x;
}

/** The year of a date literal, as SPARQL's `YEAR` reads it, or null. */
export function yearOf(t: Term): number | null {
  if (t.type !== 'literal' || !t.datatype) return null;
  if (![`${XSD}date`, `${XSD}dateTime`, `${XSD}dateTimeStamp`].includes(t.datatype)) return null;
  const m = /^\s*(-?\d{4,})-/.exec(t.value);
  return m ? Number(m[1]) : null;
}

/** RDF term equality, as `IN` compares IRIs and strings. */
function sameTerm(a: Term, b: Term): boolean {
  return toSparql(a) === toSparql(b);
}

/** Whether one of an entity's values of the brush's property is inside the brush. */
export function inBrush(values: Term[], b: Brush): boolean {
  if (b.kind === 'values') return values.some((v) => b.values.some((w) => sameTerm(v, w)));
  const [a, z] = b.range;
  return values.some((v) => {
    const x = b.kind === 'date' ? yearOf(v) : numberOf(v);
    return x !== null && x >= a && x < z;
  });
}

/**
 * The entities that every brush keeps, from the values of the brushed properties
 * (`values.get(property).get(entity)`). An entity without a value of a brushed property
 * is not kept.
 */
export function keptBy(
  ids: string[],
  brushes: Brush[],
  values: Map<string, Map<string, Term[]>>,
): Set<string> {
  const out = new Set<string>();
  for (const id of ids)
    if (brushes.every((b) => inBrush(values.get(b.property)?.get(id) ?? [], b))) out.add(id);
  return out;
}

/** The query that reads the values of `property` for a batch of entities (term keys). */
export function valuesQuery(property: string, ids: string[]): string {
  return `SELECT ?s ?v WHERE { VALUES ?s { ${ids.join(' ')} } ?s <${property}> ?v }`;
}

// ------------------------------------------------------------------ the query text ----

/** A variable name for a property's values: its local name, made unique. */
export function varFor(property: string, taken: Set<string>): string {
  let base = localName(property)
    .replace(/[^\p{L}\p{N}_]/gu, '_')
    .replace(/^_+|_+$/g, '');
  if (!base || !/^[\p{L}_]/u.test(base)) base = `v${base}`;
  base = base[0].toLowerCase() + base.slice(1);
  if (!taken.has(base)) return base;
  for (let i = 2; ; i++) if (!taken.has(`${base}${i}`)) return `${base}${i}`;
}

/**
 * The edits **Apply to query** makes for a variable `v` of a query: for each brush, a
 * triple that binds the property to a new variable and a `FILTER` over it.
 */
export function brushEdits(p: PatternOk, v: string, brushes: Brush[]): AnyEdit[] {
  const taken = new Set(patternVars(p));
  if (p.projection?.vars) for (const x of p.projection.vars) taken.add(x);
  const prefixes = p.prologue.prefixes;
  const triples: { s: string; p: string; o: string }[] = [];
  const filters: AnyEdit[] = [];
  for (const b of brushes) {
    const name = varFor(b.property, taken);
    taken.add(name);
    triples.push({ s: `?${v}`, p: compact(b.property, prefixes), o: `?${name}` });
    filters.push({ kind: 'filter-add', var: name, expr: brushCondition(b, `?${name}`) });
  }
  return triples.length ? [{ kind: 'add-many', triples }, ...filters] : [];
}

/**
 * Make several edits one after another, parsing between them, and give the changes in
 * the coordinates of `text`. Each step must parse strictly (`check`) and agree with its
 * edit (R3 of U08), or nothing is made.
 */
export function composeEdits(text: string, parse: Parse, check: Parse, edits: AnyEdit[]): Change[] {
  let cur = text;
  let a = parse(cur);
  if (!a.ok) throw new EditError(`The query does not parse: ${a.error}`);
  let set: ChangeSet | null = null;
  for (const e of edits) {
    if (!a.ok) break;
    const changes = editAny(cur, a, e, parse);
    const next = applyChanges(cur, changes);
    const strict = check(next);
    if (!strict.ok) throw new EditError(`The edit would not parse: ${strict.error}`);
    if (!agreesWith(a, e, strict))
      throw new EditError('The edit would change more of the query than it names');
    const cs = ChangeSet.of(
      changes.map((c) => ({ from: c.from, to: c.to, insert: c.insert })),
      cur.length,
    );
    set = set ? set.compose(cs) : cs;
    cur = next;
    a = strict;
  }
  const out: Change[] = [];
  (set as ChangeSet | null)?.iterChanges((fromA, toA, _fromB, _toB, inserted) =>
    out.push({ from: fromA, to: toA, insert: inserted.toString() }),
  );
  return out;
}

/**
 * The query Explore's **Apply to query** opens: the entities of the set under the
 * brushes, with their labels.
 */
export function setQuery(
  within: FacetSetSpec,
  brushes: Brush[],
  prefixes: { prefix: string; iri: string }[] = [],
): string {
  const lines: string[] = [];
  const used = new Set(['entity', 'label']);
  const term = (iri: string) => compact(iri, prefixes);
  if ('class' in within) lines.push(`?entity a ${term(within.class)} .`);
  else if ('nodes' in within) {
    const ids = within.nodes.map((n) => `<${n}>`);
    const rows: string[] = [];
    for (let i = 0; i < ids.length; i += 6) rows.push(`    ${ids.slice(i, i + 6).join(' ')}`);
    lines.push(`VALUES ?entity {\n${rows.join('\n')}\n  }`);
  }
  for (const b of brushes) {
    const v = varFor(b.property, used);
    used.add(v);
    lines.push(`?entity ${term(b.property)} ?${v} .`);
    lines.push(`FILTER(${brushCondition(b, `?${v}`)})`);
  }
  lines.push('OPTIONAL { ?entity rdfs:label ?label }');
  const declared = prefixes
    .filter((p) => lines.some((l) => l.includes(`${p.prefix}:`)))
    .map((p) => `PREFIX ${p.prefix}: <${p.iri}>`);
  if (!prefixes.some((p) => p.prefix === 'rdfs'))
    declared.push('PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>');
  return `${declared.join('\n')}\nSELECT DISTINCT ?entity ?label WHERE {\n${lines.map((l) => `  ${l}`).join('\n')}\n}\n`;
}

/** The query Explore's **Use in query** opens: the selected entities and their labels. */
export function selectionQuery(ids: string[]): string {
  const rows: string[] = [];
  for (let i = 0; i < ids.length; i += 6) rows.push(`    ${ids.slice(i, i + 6).join(' ')}`);
  return `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\nSELECT ?entity ?label WHERE {\n  VALUES ?entity {\n${rows.join('\n')}\n  }\n  OPTIONAL { ?entity rdfs:label ?label }\n}\n`;
}

/** The facet kinds as the panel names them. */
export const KIND_LABEL: Record<FacetKind, string> = {
  numeric: 'number',
  date: 'year',
  values: 'values',
};
