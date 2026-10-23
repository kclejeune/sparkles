// Arcs for a query result (docs/specs/U11-linked-views.md, section 2.2): a result with two
// columns of geometries, or two columns of entities that have geometries, draws one arc
// per row from the first column to the second, labelled by a third column when one is
// chosen. Only what the query returned is drawn: an arc needs both of its row's terms, and
// an entity's position comes from a lookup under the caller's own view.

import type { GeoConverted, Term } from './api';
import {
  abbreviate,
  isGeoLiteral,
  representativePoint,
  resolveGeometries,
  type GeoLiteral,
} from './geo';
import { shortLabel } from './graph';
import { locateEntities, type LocateDeps } from './locate';
import { MAX_ARCS, type Link, type Place } from './arcs';
import { termKey, type PrefixMap } from './rdf';
import type { Mark } from './rowdiff';

/** The columns that hold at least one IRI and no geometry literal. */
export function entityColumns(vars: string[], rows: (Term | null)[][]): string[] {
  return vars.filter((_, c) => {
    let uri = false;
    for (const r of rows) {
      const t = r[c];
      if (isGeoLiteral(t)) return false;
      if (t?.type === 'uri') uri = true;
    }
    return uri;
  });
}

/** The columns an arc can start or end in: geometries first, then entities, in column order. */
export function arcColumns(vars: string[], rows: (Term | null)[][]): string[] {
  const geo = vars.filter((_, c) => rows.some((r) => isGeoLiteral(r[c])));
  const ent = entityColumns(vars, rows);
  return [...geo, ...ent];
}

/**
 * The columns an arc starts and ends in by default: the first two geometry columns, or
 * failing that the first two entity columns. Null when the result has neither.
 */
export function defaultArcColumns(
  vars: string[],
  rows: (Term | null)[][],
): { from: string; to: string } | null {
  const geo = vars.filter((_, c) => rows.some((r) => isGeoLiteral(r[c])));
  if (geo.length >= 2) return { from: geo[0], to: geo[1] };
  const ent = entityColumns(vars, rows);
  return ent.length >= 2 ? { from: ent[0], to: ent[1] } : null;
}

/** The id of an arc's end: an IRI's term key, or a geometry literal's own text. */
export function endpointId(t: Term | null | undefined): string | null {
  if (!t) return null;
  if (t.type === 'uri') return termKey(t);
  if (isGeoLiteral(t)) return `geo:${t.datatype}\n${t.value}`;
  return null;
}

export type Ends = Map<string, { term: Term; label: string }>;

export type ResultLinks = {
  links: Link[];
  ends: Ends;
  /** Rows that could not give an arc, because a column has no entity or geometry there. */
  skipped: number;
  /** Rows left out because there are more than `MAX_ARCS`. */
  dropped: number;
};

/** The text of a labelling cell. */
function cellText(t: Term | null | undefined, prefixes: PrefixMap): string {
  if (!t) return '';
  if (t.type === 'uri') return shortLabel(t.value, prefixes);
  if (t.type === 'literal') return abbreviate(t.value, 24);
  return t.type === 'bnode' ? `_:${t.value.slice(0, 8)}` : '';
}

/** One link per row, from the `from` column to the `to` column, by the rows given. */
export function resultLinks(
  vars: string[],
  rows: (Term | null)[][],
  o: {
    from: string;
    to: string;
    label?: string;
    prefixes: PrefixMap;
    marks?: readonly Mark[];
  },
): ResultLinks {
  const fi = vars.indexOf(o.from);
  const ti = vars.indexOf(o.to);
  const li = o.label ? vars.indexOf(o.label) : -1;
  const out: ResultLinks = { links: [], ends: new Map(), skipped: 0, dropped: 0 };
  if (fi < 0 || ti < 0) return out;
  const predicate = `?${o.from} → ?${o.to}`;
  const read = rows.length > MAX_ARCS ? MAX_ARCS : rows.length;
  out.dropped = rows.length - read;
  for (let row = 0; row < read; row++) {
    const a = rows[row][fi];
    const b = rows[row][ti];
    const ida = endpointId(a);
    const idb = endpointId(b);
    if (!ida || !idb) {
      out.skipped++;
      continue;
    }
    for (const [id, t] of [
      [ida, a!],
      [idb, b!],
    ] as const)
      if (!out.ends.has(id))
        out.ends.set(id, {
          term: t,
          label:
            t.type === 'uri' ? shortLabel(t.value, o.prefixes) : abbreviate(String(t.value), 24),
        });
    const mark = o.marks?.[row];
    out.links.push({
      from: ida,
      to: idb,
      predicate,
      predicateLabel: predicate,
      label: li >= 0 ? cellText(rows[row][li], o.prefixes) || undefined : undefined,
      change: mark === 'added' || mark === 'removed' ? mark : undefined,
      row,
    });
  }
  return out;
}

export type EndPositions = {
  places: Place[];
  /** Ends that have no position: entities with no geometry, and literals that cannot be drawn. */
  missing: { id: string; label: string; iri?: string; why?: string }[];
  failed: number;
  firstError?: string;
};

/**
 * Where the ends of the arcs are: geometry literals read (in the browser or through
 * `convert`), IRIs looked up with `locateEntities`. The ends are in `ends` order.
 */
export async function locateEnds(
  ends: Ends,
  deps: LocateDeps & { convert: (b: GeoLiteral[]) => Promise<GeoConverted[]> },
): Promise<EndPositions> {
  const geo: { id: string; lit: GeoLiteral }[] = [];
  const iris: string[] = [];
  for (const [id, e] of ends) {
    if (isGeoLiteral(e.term))
      geo.push({ id, lit: { value: e.term.value, datatype: e.term.datatype } });
    else if (e.term.type === 'uri') iris.push(e.term.value);
  }
  const at = new Map<string, [number, number]>();
  const why = new Map<string, string>();
  if (geo.length) {
    const res = await resolveGeometries(
      geo.map((g) => g.lit),
      deps.convert,
    );
    geo.forEach((g, i) => {
      const r = res[i];
      if (r && 'geometry' in r) {
        const p = representativePoint(r.geometry);
        if (p) at.set(g.id, p);
      } else if (r && 'error' in r) why.set(g.id, r.error);
    });
  }
  const located = iris.length
    ? await locateEntities(iris, deps)
    : { found: new Map<string, [number, number]>(), failed: 0 };
  const places: Place[] = [];
  const missing: EndPositions['missing'] = [];
  for (const [id, e] of ends) {
    const p = e.term.type === 'uri' ? located.found.get(e.term.value) : at.get(id);
    if (p) places.push({ id, label: e.label, lon: p[0], lat: p[1] });
    else
      missing.push({
        id,
        label: e.label,
        iri: e.term.type === 'uri' ? e.term.value : undefined,
        why: why.get(id),
      });
  }
  return {
    places,
    missing,
    failed: located.failed,
    firstError: 'firstError' in located ? located.firstError : undefined,
  };
}
