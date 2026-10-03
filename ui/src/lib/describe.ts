// The dataset page's DESCRIBE setting (`/$/describe/{ds}`): the form's state, the body a
// save sends, and a one-line summary.

import type { DescribeMode, DescribeOptions, DescribeSetting } from './api';

/** What each mode describes, in words. */
export const MODE_TEXT: Record<DescribeMode, string> = {
  cbd: 'Concise Bounded Description: outgoing triples, following blank nodes',
  scbd: 'Symmetric CBD: outgoing and incoming triples, following blank nodes',
  outgoing: 'Outgoing triples only, as Fuseki describes by default',
};

/** The form: limits as text, so that an empty field means no limit. */
export type DescribeForm = {
  mode: DescribeMode;
  labels: boolean;
  reifiers: boolean;
  maxTriples: string;
  maxDepth: string;
};

export function describeForm(s: DescribeOptions): DescribeForm {
  return {
    mode: s.mode,
    labels: s.labels,
    reifiers: s.reifiers,
    maxTriples: s.maxTriples == null ? '' : String(s.maxTriples),
    maxDepth: s.maxDepth == null ? '' : String(s.maxDepth),
  };
}

/** A limit field: empty or 0 is no limit; anything but a whole number is an error. */
function limit(field: string, label: string): number | null | string {
  const t = field.trim();
  if (t === '') return null;
  if (!/^\d+$/.test(t)) return `${label} must be a whole number, or empty for no limit`;
  const n = Number(t);
  if (!Number.isSafeInteger(n)) return `${label} is too large`;
  return n === 0 ? null : n;
}

/** The `PUT` body of a form, or the error that keeps it from being saved. */
export function describeBody(f: DescribeForm): { ok: DescribeOptions } | { error: string } {
  const maxTriples = limit(f.maxTriples, 'Max triples');
  if (typeof maxTriples === 'string') return { error: maxTriples };
  const maxDepth = limit(f.maxDepth, 'Max depth');
  if (typeof maxDepth === 'string') return { error: maxDepth };
  if (maxDepth != null && maxDepth > 4294967295) return { error: 'Max depth is too large' };
  return {
    ok: { mode: f.mode, labels: f.labels, reifiers: f.reifiers, maxTriples, maxDepth },
  };
}

/** Whether the form differs from the setting. */
export function describeChanged(f: DescribeForm, s: DescribeOptions): boolean {
  const b = describeBody(f);
  if ('error' in b) return true;
  const o = b.ok;
  return (
    o.mode !== s.mode ||
    o.labels !== s.labels ||
    o.reifiers !== s.reifiers ||
    o.maxTriples !== (s.maxTriples ?? null) ||
    o.maxDepth !== (s.maxDepth ?? null)
  );
}

/** The setting in one line, such as `scbd, with labels, at most 500 triples`. */
export function describeSummary(s: DescribeSetting | DescribeOptions): string {
  const parts: string[] = [s.mode];
  if (s.labels) parts.push('with labels');
  if (s.reifiers) parts.push('with reifiers');
  if (s.maxTriples != null) parts.push(`at most ${s.maxTriples} triples`);
  if (s.maxDepth != null) parts.push(`depth ${s.maxDepth}`);
  return parts.join(', ');
}
