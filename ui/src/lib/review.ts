// The review inbox and the branch review (C18 §7.10, §8.9): which facts **Accept all
// that pass** selects, how a source's text splits around the cited spans, and the labels
// of signals and branch kinds.

import type { InboxBranch, ReviewFact, Signal, Signals } from './review-api';

/** A stable key of a fact in a listing. */
export const factKey = (f: Pick<ReviewFact, 's' | 'p' | 'o' | 'graph'>) =>
  `${f.s} ${f.p} ${f.o} ${f.graph}`;

/**
 * The keys **Accept all that pass** selects: facts whose span, link and guard signals
 * pass, and with `corroborated` also the corroboration signal. Confidence plays no part.
 */
export function passingKeys(facts: ReviewFact[], corroborated = false): Set<string> {
  const out = new Set<string>();
  for (const f of facts) {
    const s = f.signals;
    if (!s) continue;
    if (s.span !== 'pass' || s.link !== 'pass' || s.guard !== 'pass') continue;
    if (corroborated && s.corroboration !== 'pass') continue;
    out.add(factKey(f));
  }
  return out;
}

/** The signals in the order the inbox shows them, with their labels. */
export const SIGNALS: { key: keyof Signals; label: string }[] = [
  { key: 'span', label: 'span' },
  { key: 'link', label: 'link' },
  { key: 'guard', label: 'guard' },
  { key: 'corroboration', label: 'corroborated' },
];

/** The text of a signal's badge. */
export function signalText(key: keyof Signals, s: Signal, candidates = 0): string {
  if (key === 'span' && s === 'none') return 'no span';
  if (key === 'link' && s === 'fail' && candidates > 0)
    return `link: ${candidates} candidate${candidates === 1 ? '' : 's'}`;
  if (key === 'corroboration') return s === 'pass' ? 'corroborated' : '';
  const mark = s === 'pass' ? '✓' : s === 'fail' ? '✗' : '?';
  return `${SIGNALS.find((x) => x.key === key)?.label ?? key} ${mark}`;
}

/** The badge class of a signal. */
export const signalClass = (s: Signal) =>
  s === 'pass' ? 'ok' : s === 'fail' ? 'danger' : s === 'none' ? 'spark' : '';

/** The words of a branch kind. */
export const KIND_LABELS: Record<InboxBranch['kind'], string> = {
  ingest: 'Ingest',
  review: 'Review',
  inbox: 'Held facts',
  consolidation: 'Consolidation',
  proposal: 'Proposal',
};

/** A piece of a source's text: plain, or inside the spans of the listed facts. */
export type Segment = { text: string; start: number; end: number; facts: number[] };

/**
 * The text split at every span boundary, each piece with the indexes of the facts whose
 * span covers it. Offsets count code points, as the server's do.
 */
export function segments(
  text: string,
  spans: { start: number; end: number; fact: number }[],
): Segment[] {
  const chars = Array.from(text);
  const cuts = new Set<number>([0, chars.length]);
  for (const s of spans) {
    if (s.start >= 0 && s.end <= chars.length && s.start < s.end) {
      cuts.add(s.start);
      cuts.add(s.end);
    }
  }
  const points = [...cuts].sort((a, b) => a - b);
  const out: Segment[] = [];
  for (let i = 0; i + 1 < points.length; i++) {
    const [a, b] = [points[i], points[i + 1]];
    const facts = spans.filter((s) => s.start <= a && s.end >= b).map((s) => s.fact);
    out.push({ text: chars.slice(a, b).join(''), start: a, end: b, facts });
  }
  return out;
}

/** The IRI inside `<…>`, or the text as it is. */
export const bare = (t: string) => (t.startsWith('<') && t.endsWith('>') ? t.slice(1, -1) : t);

/**
 * The page of a converted PDF that holds code point `offset`: the last page whose start
 * is at or before it. `undefined` when the source has no pages.
 */
export function pageOf(
  pages: { page: number; start: number }[] | undefined,
  offset: number,
): number | undefined {
  let found: number | undefined;
  for (const p of pages ?? []) {
    if (p.start <= offset) found = p.page;
    else break;
  }
  return found;
}

/** A list of page numbers as text: `3`, `3 and 5`, `1, 2 and 4`. */
export function pageList(pages: number[]): string {
  if (pages.length <= 1) return pages.join('');
  return `${pages.slice(0, -1).join(', ')} and ${pages[pages.length - 1]}`;
}

/** The words of an ingestion's status. */
export const INGEST_STATUS: Record<string, string> = {
  queued: 'Queued',
  converting: 'Converting',
  registering: 'Registering the source',
  'awaiting-confirmation': 'Waiting for your confirmation',
  extracting: 'Extracting facts',
  linking: 'Linking entities',
  writing: 'Writing proposals',
  'awaiting-approval': 'Waiting for your approval',
  done: 'Done',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

/** The review branch's page. */
export const reviewHref = (base: string, ds: string, branch: string) =>
  `${base}/datasets/${encodeURIComponent(ds)}/review/${encodeURIComponent(branch)}`;
