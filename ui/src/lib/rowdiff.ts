// Bag diffs of query results for the time slider's compare mode (docs/specs/U01-time-slider.md
// §4.2). Two results are compared as bags of rows, as C18 §6.7.5 compares them: a row is
// its terms in column order, and a row that occurs twice on one side and once on the other
// is one added or removed row.
import type { SparqlDiff, Term } from './api';
import { termKey } from './rdf';

export type Row = (Term | null)[];
export type Triple = [Term, Term, Term];

/** Whether a row exists only in the right result (`added`), only in the left (`removed`), or in both. */
export type Mark = 'added' | 'removed' | 'kept';

/** The most rows per side that the browser compares; beyond it only counts are shown. */
export const ROW_DIFF_LIMIT = 10_000;

const UNBOUND = '\u0001';
const SEP = '\u0000';

/** The identity of a row: its terms in order, an unbound cell apart from an empty literal. */
export function rowKey(row: readonly (Term | null)[]): string {
  return row.map((t) => (t ? termKey(t) : UNBOUND)).join(SEP);
}

export type RowDiff = {
  /** The right result's rows in order, then the rows only the left one has. */
  rows: Row[];
  marks: Mark[];
  added: number;
  removed: number;
  unchanged: number;
};

/** The rows are too many to compare in the browser. */
export type TooLarge = { tooLarge: true; left: number; right: number };

/** Compare `left` (the earlier state) with `right` as bags. */
export function diffRows(left: readonly Row[], right: readonly Row[]): RowDiff {
  // how many of each row the left result still has to match against the right one
  const pending = new Map<string, number>();
  for (const r of left) {
    const k = rowKey(r);
    pending.set(k, (pending.get(k) ?? 0) + 1);
  }
  const rows: Row[] = [];
  const marks: Mark[] = [];
  let added = 0;
  let unchanged = 0;
  for (const r of right) {
    const k = rowKey(r);
    const n = pending.get(k) ?? 0;
    rows.push(r);
    if (n > 0) {
      pending.set(k, n - 1);
      marks.push('kept');
      unchanged++;
    } else {
      marks.push('added');
      added++;
    }
  }
  // what is still pending was in the left result only; those rows keep the left order,
  // and the last occurrences of a repeated row are the ones that remain
  const skip = new Map<string, number>();
  for (const r of left) {
    const k = rowKey(r);
    skip.set(k, (skip.get(k) ?? 0) + 1);
  }
  let removed = 0;
  for (const r of left) {
    const k = rowKey(r);
    const total = skip.get(k) ?? 0;
    skip.set(k, total - 1);
    // the first (total - pending) occurrences were matched; the rest are removed
    if (total <= (pending.get(k) ?? 0)) {
      rows.push(r);
      marks.push('removed');
      removed++;
    }
  }
  return { rows, marks, added, removed, unchanged };
}

/** Compare two results' rows, or say they are too many for the browser to compare. */
export function compareRows(
  left: readonly Row[],
  right: readonly Row[],
  limit = ROW_DIFF_LIMIT,
): RowDiff | TooLarge {
  if (left.length > limit || right.length > limit)
    return { tooLarge: true, left: left.length, right: right.length };
  return diffRows(left, right);
}

export const isTooLarge = (d: RowDiff | TooLarge): d is TooLarge => 'tooLarge' in d;

/** The same comparison for a graph result: the triples as bags. */
export function diffTriples(
  left: readonly Triple[],
  right: readonly Triple[],
): Omit<RowDiff, 'rows'> & { rows: Triple[] } {
  const d = diffRows(left as readonly Row[], right as readonly Row[]);
  return { ...d, rows: d.rows as Triple[] };
}

/** A filter chip of the change column. */
export type ChangeFilter = 'all' | 'added' | 'removed' | 'unchanged';

export const CHANGE_FILTERS: readonly ChangeFilter[] = ['all', 'added', 'removed', 'unchanged'];

const WANTED: Record<Exclude<ChangeFilter, 'all'>, Mark> = {
  added: 'added',
  removed: 'removed',
  unchanged: 'kept',
};

/** Keep only the rows the chip names. */
export function filterDiff(
  d: Pick<RowDiff, 'rows' | 'marks'>,
  filter: ChangeFilter,
): Pick<RowDiff, 'rows' | 'marks'> {
  if (filter === 'all') return { rows: d.rows, marks: d.marks };
  const want = WANTED[filter];
  const rows: Row[] = [];
  const marks: Mark[] = [];
  d.rows.forEach((r, i) => {
    if (d.marks[i] === want) {
      rows.push(r);
      marks.push(d.marks[i]);
    }
  });
  return { rows, marks };
}

/** "Commit 3: 1 row added, 1 removed" for the slider's live region. */
export function rowAnnouncement(
  label: string,
  d: Pick<RowDiff, 'added' | 'removed'> | null,
  noun = 'row',
): string {
  if (!d) return label;
  const n = (c: number) => `${c} ${noun}${c === 1 ? '' : 's'}`;
  if (!d.added && !d.removed) return `${label}: no ${noun}s changed`;
  const parts: string[] = [];
  if (d.added) parts.push(`${n(d.added)} added`);
  if (d.removed) parts.push(d.added ? `${d.removed} removed` : `${n(d.removed)} removed`);
  return `${label}: ${parts.join(', ')}`;
}

/** The sign shown in the change column. */
export const MARK_SIGN: Record<Mark, string> = { added: '+', removed: '−', kept: '' };

/** The words for a mark, for screen readers and tooltips. */
export const MARK_WORD: Record<Mark, string> = {
  added: 'added',
  removed: 'removed',
  kept: 'unchanged',
};

/**
 * The rows a server comparison lists, as a table: the rows only the earlier state has, then
 * the rows only the later one has, each marked. A list the server cut is shorter than its
 * count.
 */
export function serverRows(d: Pick<SparqlDiff, 'vars' | 'addedRows' | 'removedRows'>): {
  vars: string[];
  rows: Row[];
  marks: Mark[];
} {
  const row = (r: Partial<Record<string, Term>>): Row => d.vars.map((v) => r[v] ?? null);
  return {
    vars: d.vars,
    rows: [...d.removedRows.map(row), ...d.addedRows.map(row)],
    marks: [...d.removedRows.map((): Mark => 'removed'), ...d.addedRows.map((): Mark => 'added')],
  };
}

/** "1,204 rows added, 87 removed, 18,000 unchanged", and what the lists hold. */
export function serverSummary(
  d: Pick<SparqlDiff, 'added' | 'removed' | 'same' | 'addedRows' | 'removedRows' | 'truncated'>,
): string {
  const fmt = (n: number) => n.toLocaleString('en-US');
  const head = `${fmt(d.added)} ${d.added === 1 ? 'row' : 'rows'} added, ${fmt(d.removed)} removed, ${fmt(d.same)} unchanged`;
  if (!d.truncated.added && !d.truncated.removed) return head;
  const cut = [
    d.truncated.added ? `${fmt(d.addedRows.length)} of the added` : '',
    d.truncated.removed ? `${fmt(d.removedRows.length)} of the removed` : '',
  ].filter(Boolean);
  return `${head}. Listing ${cut.join(' and ')}`;
}
