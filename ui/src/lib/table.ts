// Result-table sorting (kept out of ResultTable.svelte so it can be unit-tested).
import type { Term } from './api';
import { displayTerm, toSparql, type PrefixMap } from './rdf';

export type SortSpec = { col: number; dir: 1 | -1 };

const NUMERIC =
  /#(integer|decimal|double|float|long|int|short|byte|nonNegativeInteger|positiveInteger|unsignedInt|unsignedLong)$/;

/** Sort key of a cell: numbers for numeric literals, display text otherwise, null if unbound. */
export function sortKey(t: Term | null, prefixes: PrefixMap): number | string | null {
  if (!t) return null;
  if (t.type === 'literal' && t.datatype && NUMERIC.test(t.datatype)) {
    const n = Number(t.value);
    if (!Number.isNaN(n)) return n;
  }
  return t.type === 'uri' ? displayTerm(t, prefixes) : t.type === 'triple' ? toSparql(t) : t.value;
}

/** Row indices in display order. Unbound cells sort last in both directions; ties keep row order. */
export function sortedOrder(
  rows: (Term | null)[][],
  sort: SortSpec | null,
  prefixes: PrefixMap,
): number[] {
  const idx = Array.from({ length: rows.length }, (_, i) => i);
  if (!sort) return idx;
  const { col, dir } = sort;
  const keys = rows.map((r) => sortKey(r[col] ?? null, prefixes));
  const coll = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
  idx.sort((a, b) => {
    const ka = keys[a];
    const kb = keys[b];
    if (ka === kb) return a - b;
    if (ka == null) return 1;
    if (kb == null) return -1;
    if (typeof ka === 'number' && typeof kb === 'number') return (ka - kb) * dir;
    return coll.compare(String(ka), String(kb)) * dir;
  });
  return idx;
}
