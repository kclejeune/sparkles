// Completion of Manchester syntax (docs/specs/U12-ontology-workbench/SPEC.md): the name
// being typed before the cursor, and the entity names and keywords that complete it,
// with names that need quotes written in quotes.
import type { Completion } from '@codemirror/autocomplete';
import { manchesterName } from './ontology';

/** An entity name to complete: its label or prefixed name, its IRI and its kind. */
export type NameOption = { name: string; iri: string; kind: string };

/** The keywords of class expressions that completion offers. */
const KEYWORDS = [
  'and',
  'or',
  'not',
  'that',
  'some',
  'only',
  'value',
  'Self',
  'min',
  'max',
  'exactly',
  'inverse',
];

/**
 * The name being typed at the end of `before`: a quoted name still open, such as
 * `'Grad`, or a bare word. Null after a space or a bracket.
 */
export function completionWord(before: string): string | null {
  // an open quote: an odd number of unescaped quotes
  let open = -1;
  for (let i = 0; i < before.length; i++) {
    const c = before[i];
    if (c === '\\' && open >= 0) {
      i += 1;
      continue;
    }
    if (c === "'") open = open >= 0 ? -1 : i;
    else if (c === '"' && open < 0) {
      // skip a literal
      const end = before.indexOf('"', i + 1);
      if (end < 0) return null;
      i = end;
    }
  }
  if (open >= 0) return before.slice(open);
  const m = /[^\s()[\]{},'"<>]+$/.exec(before);
  return m ? m[0] : null;
}

/** The completions of `word` among `names` and the keywords, best first, at most 50. */
export function manchesterCompletions(word: string, names: NameOption[]): Completion[] {
  const quoted = word.startsWith("'");
  const typed = (quoted ? word.slice(1) : word).toLowerCase();
  const scored: { c: Completion; rank: number }[] = [];
  const seen = new Set<string>();
  for (const n of names) {
    const lower = n.name.toLowerCase();
    const at = lower.indexOf(typed);
    if (typed && at < 0) continue;
    const apply = manchesterName(n.name, n.iri);
    const key = `${apply}|${n.iri}`;
    if (seen.has(key)) continue;
    seen.add(key);
    scored.push({
      c: { label: n.name, apply, detail: n.kind, info: n.iri, type: 'variable' },
      rank: at === 0 ? 0 : 1,
    });
  }
  if (!quoted)
    for (const k of KEYWORDS)
      if (typed && k.toLowerCase().startsWith(typed))
        scored.push({ c: { label: k, type: 'keyword', detail: 'keyword' }, rank: 0 });
  scored.sort((a, b) => a.rank - b.rank || a.c.label.localeCompare(b.c.label));
  return scored.slice(0, 50).map((s) => s.c);
}
