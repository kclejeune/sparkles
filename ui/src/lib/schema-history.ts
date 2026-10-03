// Helpers of the schema browser's class profiles and schema diffs: the lines a profile
// row shows, the states a diff accepts, and the text of each change.

import type { ProfileProperty, SchemaChange, SchemaDiff } from './api';
import { fmtInt } from './format';

/** Shortens an IRI for display (a prefixed name or a local name). */
export type Shorten = (iri: string) => string;

/** A class expression with its `<IRI>`s shortened. */
export function shortenExpression(text: string, short: Shorten): string {
  return text.replace(/<([^<>\s]+)>/g, (_, iri: string) => short(iri));
}

/** "2 of 3 (67%)": the instances with a value among all instances of the class. */
export function coverage(p: ProfileProperty, instances: number): string {
  if (instances <= 0) return fmtInt(p.instances);
  const pct = Math.round((100 * p.instances) / instances);
  return `${fmtInt(p.instances)} of ${fmtInt(instances)} (${pct}%)`;
}

/** "1" or "1–3": the fewest and the most values of one instance that has any. */
export function valuesRange(p: ProfileProperty): string {
  return p.minPerInstance === p.maxPerInstance
    ? fmtInt(p.minPerInstance)
    : `${fmtInt(p.minPerInstance)}–${fmtInt(p.maxPerInstance)}`;
}

/** The kinds of the values with their triples, most first: "IRI 3 · xsd:string 2". */
export function objectKinds(p: ProfileProperty, short: Shorten): string {
  const parts: [string, number][] = [];
  if (p.objects.iri) parts.push(['IRI', p.objects.iri]);
  if (p.objects.blank) parts.push(['blank node', p.objects.blank]);
  if (p.objects.tripleTerm) parts.push(['triple term', p.objects.tripleTerm]);
  for (const l of p.objects.literals ?? []) parts.push([short(l.datatype), l.triples]);
  return parts
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([k, n]) => `${k} ${fmtInt(n)}`)
    .join(' · ');
}

/**
 * A state as `from=`/`to=` take it, from what was typed: a commit number, `commit:N`,
 * `time:<RFC 3339>` or `snapshot:NAME`; null when it is none of these.
 */
export function parseAt(text: string): string | null {
  const t = text.trim();
  if (/^\d{1,20}$/.test(t)) return t;
  if (/^commit:\d{1,20}$/.test(t)) return t;
  if (/^snapshot:[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(t)) return t;
  if (/^time:\S+$/.test(t) && !Number.isNaN(Date.parse(t.slice(5)))) return t;
  return null;
}

/** A value of a change: an IRI shortened, a label with its language, a number. */
export function changeValue(v: unknown, short: Shorten): string {
  if (v == null) return '–';
  if (typeof v === 'string') return /^[a-z][a-z0-9+.-]*:/i.test(v) ? short(v) : v;
  if (typeof v === 'number') return fmtInt(v);
  if (typeof v === 'object' && !Array.isArray(v) && 'value' in v) {
    const l = v as { value: string; lang?: string };
    return l.lang ? `"${l.value}"@${l.lang}` : `"${l.value}"`;
  }
  if (Array.isArray(v)) return v.map((x) => changeValue(x, short)).join(' ⊑ ');
  return JSON.stringify(v);
}

/** The path of a change with its group keys shortened: `literals[xsd:integer].triples`. */
export function changePath(path: string, short: Shorten): string {
  return path.replace(/\[([a-z]+)=([^\]]*)\]/g, (_, k: string, v: string) =>
    k === 'datatype' || k === 'class' || k === 'iri' ? `[${short(v)}]` : `[${v}]`,
  );
}

/** One change as text: "observed.instances 1 → 2" or "declared.superClasses +ex:A −ex:B". */
export function changeText(c: SchemaChange, short: Shorten): string {
  const path = changePath(c.path, short);
  if ('added' in c) {
    const parts = [
      ...c.added.map((v) => `+${changeValue(v, short)}`),
      ...c.removed.map((v) => `−${changeValue(v, short)}`),
    ];
    return `${path} ${parts.join(' ')}`;
  }
  return `${path} ${changeValue(c.from, short)} → ${changeValue(c.to, short)}`;
}

function counted(n: number, one: string, many: string): string {
  return `${fmtInt(n)} ${n === 1 ? one : many}`;
}

/** "1 class added, 2 changed · 1 predicate removed", or "No changes". */
export function diffSummary(d: SchemaDiff): string {
  const c = d.counts;
  const part = (added: number, removed: number, changed: number, one: string, many: string) => {
    const bits: string[] = [];
    if (added) bits.push(`${counted(added, one, many)} added`);
    if (removed)
      bits.push(`${bits.length ? fmtInt(removed) : counted(removed, one, many)} removed`);
    if (changed)
      bits.push(`${bits.length ? fmtInt(changed) : counted(changed, one, many)} changed`);
    return bits.join(', ');
  };
  const parts = [
    part(c.classesAdded, c.classesRemoved, c.classesChanged, 'class', 'classes'),
    part(c.predicatesAdded, c.predicatesRemoved, c.predicatesChanged, 'predicate', 'predicates'),
  ].filter(Boolean);
  if (!parts.length && !d.report.length) return 'No changes';
  return parts.length ? parts.join(' · ') : 'Only totals changed';
}
