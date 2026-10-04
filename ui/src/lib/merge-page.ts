// The merge page (docs/specs/F09-branches-and-merges.md §2.9): conflicts grouped by graph
// and subject, the choices made for them, and the resolutions those choices send.
import type { ConflictCell, GuardReport, MergeRequest, Resolution } from './api';
import { fmtInt } from './format';
import { ntShort } from './branches';
import type { PrefixMap } from './rdf';

/** What a choice takes for a conflict. `objects` needs values typed in, so the page leaves it out. */
export type Take = Exclude<Resolution['take'], 'objects'>;

export const TAKES: { take: Take; label: string; title: string }[] = [
  { take: 'ours', label: 'Ours', title: "Keep the target's values" },
  { take: 'theirs', label: 'Theirs', title: "Take the source's values" },
  { take: 'base', label: 'Base', title: 'Put back the values of the merge base' },
  { take: 'union', label: 'Both', title: "Keep both sides' values" },
];

/** The key of a conflict: its graph, subject and predicate. */
export const cellKey = (c: Pick<ConflictCell, 'graph' | 'subject' | 'predicate'>) =>
  `${c.graph ?? ''}\u0000${c.subject}\u0000${c.predicate ?? ''}`;

/** The key of a subject group: its graph and subject. */
export const groupKey = (graph: string | null, subject: string) => `${graph ?? ''}\u0000${subject}`;

export type SubjectGroup = { key: string; subject: string; cells: ConflictCell[] };
export type GraphGroup = { graph: string | null; subjects: SubjectGroup[]; count: number };

/** The conflicts grouped by graph (the default graph first), then by subject, in report order. */
export function groupConflicts(cells: ConflictCell[]): GraphGroup[] {
  const graphs = new Map<string, GraphGroup>();
  for (const c of cells) {
    const gk = c.graph ?? '';
    let g = graphs.get(gk);
    if (!g) graphs.set(gk, (g = { graph: c.graph ?? null, subjects: [], count: 0 }));
    const sk = groupKey(c.graph ?? null, c.subject);
    let s = g.subjects.find((x) => x.key === sk);
    if (!s) g.subjects.push((s = { key: sk, subject: c.subject, cells: [] }));
    s.cells.push(c);
    g.count++;
  }
  return [...graphs.values()].sort((a, b) =>
    a.graph == null ? -1 : b.graph == null ? 1 : a.graph.localeCompare(b.graph),
  );
}

/** The choices made on the page: per subject group and per cell, by key. */
export type Choices = { groups: Record<string, Take>; cells: Record<string, Take> };

export const noChoices = (): Choices => ({ groups: {}, cells: {} });

/** What resolves a cell: its own choice, its group's, or null. */
export function choiceOf(c: ConflictCell, ch: Choices): Take | null {
  return ch.cells[cellKey(c)] ?? ch.groups[groupKey(c.graph ?? null, c.subject)] ?? null;
}

/** The resolutions the choices send, groups first: the server applies the most specific. */
export function resolutionsOf(cells: ConflictCell[], ch: Choices): Resolution[] {
  const out: Resolution[] = [];
  const groups = new Set<string>();
  for (const c of cells) {
    const gk = groupKey(c.graph ?? null, c.subject);
    const g = ch.groups[gk];
    if (g && !groups.has(gk)) {
      groups.add(gk);
      out.push({ graph: c.graph ?? null, subject: c.subject, take: g });
    }
  }
  for (const c of cells) {
    const t = ch.cells[cellKey(c)];
    if (!t) continue;
    const r: Resolution = { graph: c.graph ?? null, subject: c.subject, take: t };
    if (c.predicate) r.predicate = c.predicate;
    out.push(r);
  }
  return out;
}

/** Drop the choices for conflicts the report no longer lists (after the heads moved). */
export function keepChoices(cells: ConflictCell[], ch: Choices): Choices {
  const cks = new Set(cells.map(cellKey));
  const gks = new Set(cells.map((c) => groupKey(c.graph ?? null, c.subject)));
  return {
    groups: Object.fromEntries(Object.entries(ch.groups).filter(([k]) => gks.has(k))),
    cells: Object.fromEntries(Object.entries(ch.cells).filter(([k]) => cks.has(k))),
  };
}

/**
 * The conflicts that no choice resolves: the listed cells without a choice, plus those the
 * report left out (`total` counts them all). A rule for every other conflict
 * (`onConflict`) resolves them all.
 */
export function unresolved(
  cells: ConflictCell[],
  total: number,
  ch: Choices,
  onConflict: MergeRequest['onConflict'],
): number {
  if (onConflict && onConflict !== 'fail') return 0;
  const open = cells.filter((c) => !choiceOf(c, ch)).length;
  return open + Math.max(0, total - cells.length);
}

/** The values a cell ends with under `take`, for the "Result" column. */
export function resultObjects(c: ConflictCell, take: Take | null): string[] | null {
  switch (take) {
    case 'ours':
      return c.ours;
    case 'theirs':
      return c.theirs;
    case 'base':
      return c.base;
    case 'union':
      return [...new Set([...c.ours, ...c.theirs])].sort();
    default:
      return null;
  }
}

/** A graph group's heading: "Default graph" or the graph's name. */
export function graphHeading(graph: string | null, prefixes: PrefixMap = {}): string {
  return graph == null ? 'Default graph' : `Graph ${ntShort(graph, prefixes)}`;
}

/** The URL of the merge page for `source` into `target` (the app's base path included). */
export function mergeHref(datasetPath: string, source: string, target: string): string {
  const q = new URLSearchParams({ source, target });
  return `${datasetPath}/merge?${q}`;
}

/** A term of a guard result (SPARQL JSON) as text. */
function term(t: unknown): string {
  const x = (t ?? {}) as { type?: string; value?: string };
  if (x.value == null) return '';
  if (x.type === 'uri') return `<${x.value}>`;
  if (x.type === 'bnode') return `_:${x.value}`;
  if (x.type === 'path') return x.value;
  return JSON.stringify(x.value);
}

/** One row of a guard report: focus node, path, message and shape. */
export type GuardRow = { node: string; path: string; message: string; shape: string };

/** The rows of a guard report, SHACL results or ShEx associations. */
export function guardRows(v: GuardReport, prefixes: PrefixMap = {}): GuardRow[] {
  const short = (t: string) => (t ? ntShort(t, prefixes) : '');
  return (v.results ?? []).map((r) => {
    if ('focusNode' in r) {
      const msgs = (r.messages as string[] | undefined) ?? [];
      const comp = term(r.sourceConstraintComponent).replace(/^<.*#(.*)>$/, '$1');
      return {
        node: short(term(r.focusNode)),
        path: short(term(r.resultPath)),
        message: msgs[0] ?? comp,
        shape: short(term(r.sourceShape)),
      };
    }
    return {
      node: short(term(r.node)),
      path: '',
      message: String(r.reason ?? r.status ?? ''),
      shape: short(term(r.shape)),
    };
  });
}

/** "2 blocking results" of a guard report. */
export function guardSummary(v: GuardReport): string {
  const n = v.blocking ?? v.results?.length ?? 0;
  return `${fmtInt(n)} blocking result${n === 1 ? '' : 's'}`;
}
