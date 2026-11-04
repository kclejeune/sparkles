// A diff read as a change set (ROADMAP item 633): the quads of `GET /{ds}/diff`, which name
// their terms in N-Triples syntax, grouped by subject, with each change a property and a
// value that the ChangeSet component names by label. The Workbench's history, the change
// views of sessions and memory, and any other view of a commit use it.

import type { DiffQuad } from './api';

/** One term of a diff quad. */
export type CsTerm =
  | { type: 'iri'; value: string }
  | { type: 'bnode'; value: string }
  | { type: 'literal'; value: string; lang?: string; datatype?: string };

/** One change: added or removed, of a property and a value. */
export type CsChange = { op: '+' | '-'; predicate: string; object: CsTerm; graph: string | null };

/** The changes of one subject, removals before additions within each property. */
export type CsGroup = {
  subject: CsTerm;
  added: number;
  removed: number;
  changes: CsChange[];
};

/** A term written in N-Triples syntax, as the diff quads give them. */
export function parseNt(s: string): CsTerm {
  const t = s.trim();
  if (t.startsWith('<') && t.endsWith('>')) return { type: 'iri', value: t.slice(1, -1) };
  if (t.startsWith('_:')) return { type: 'bnode', value: t.slice(2) };
  if (t.startsWith('"')) {
    // the closing quote is the last one not escaped
    let end = t.length - 1;
    while (end > 0 && !(t[end] === '"' && t[end - 1] !== '\\')) end--;
    if (end <= 0) return { type: 'literal', value: t };
    const raw = t.slice(0, end + 1);
    let value: string;
    try {
      value = JSON.parse(raw);
    } catch {
      value = raw.slice(1, -1);
    }
    const rest = t.slice(end + 1);
    if (rest.startsWith('@')) return { type: 'literal', value, lang: rest.slice(1) };
    if (rest.startsWith('^^<') && rest.endsWith('>'))
      return { type: 'literal', value, datatype: rest.slice(3, -1) };
    return { type: 'literal', value };
  }
  return { type: 'iri', value: t };
}

/** The quads grouped by subject in the order subjects first appear, each sorted by property. */
export function changeGroups(quads: readonly DiffQuad[]): CsGroup[] {
  const groups = new Map<string, CsGroup>();
  for (const q of quads) {
    let g = groups.get(q.subject);
    if (!g) {
      g = { subject: parseNt(q.subject), added: 0, removed: 0, changes: [] };
      groups.set(q.subject, g);
    }
    if (q.op === '+') g.added++;
    else g.removed++;
    g.changes.push({
      op: q.op,
      predicate: parseNt(q.predicate).value,
      object: parseNt(q.object),
      graph: q.graph,
    });
  }
  for (const g of groups.values())
    g.changes.sort(
      (a, b) =>
        a.predicate.localeCompare(b.predicate) || (a.op === b.op ? 0 : a.op === '-' ? -1 : 1),
    );
  return [...groups.values()];
}

/** The subjects a set of quads names, as IRIs, for one label lookup. */
export function changeIris(quads: readonly DiffQuad[]): string[] {
  const out = new Set<string>();
  for (const q of quads)
    for (const t of [q.subject, q.predicate, q.object]) {
      const p = parseNt(t);
      if (p.type === 'iri') out.add(p.value);
    }
  return [...out];
}
