// Point-in-time reads, named snapshots and diffs (docs/API.md, "Point-in-time reads and
// snapshots").
import type { AtInfo, Commit, DiffQuad } from './api';
import { fmtInt, fmtRelative } from './format';

/** A snapshot name: a letter or digit, then letters, digits, `.`, `_` or `-`, 64 at most. */
export function validSnapshotName(n: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(n);
}

/**
 * The canonical form of an `at` selector typed by a user, or null for the head (empty
 * input or `head`). Throws an Error naming the accepted forms for anything else.
 */
export function normalizeAt(input: string): string | null {
  const s = input.trim();
  if (!s || s === 'head') return null;
  if (/^\d{1,20}$/.test(s)) return `commit:${Number(s)}`;
  const commit = /^commit:(\d{1,20})$/.exec(s);
  if (commit) return `commit:${Number(commit[1])}`;
  const snap = /^snapshot:(.+)$/.exec(s);
  if (snap && validSnapshotName(snap[1])) return s;
  const time = /^time:(.+)$/.exec(s);
  if (time && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$/i.test(time[1])) {
    if (!Number.isNaN(new Date(time[1]).getTime())) return s;
  }
  throw new Error('Use head, a commit number, commit:N, time:<RFC 3339> or snapshot:<name>');
}

/** Whether `input` is a selector `normalizeAt` accepts. */
export function validAt(input: string): boolean {
  try {
    normalizeAt(input);
    return true;
  } catch {
    return false;
  }
}

/**
 * The note shown next to a result read at a past state: "at commit 42 · 3 h ago", or
 * "at commit 57 (head)" when the selector named the current state.
 */
export function atLabel(at: AtInfo, now = Date.now()): string {
  const commit = at.commit != null ? `commit ${fmtInt(at.commit)}` : at.selector;
  if (!at.historical) return `at ${commit} (head)`;
  const when = at.datetime ? ` · ${fmtRelative(at.datetime, now)}` : '';
  const behind =
    at.head != null && at.commit != null && at.head > at.commit
      ? ` · ${fmtInt(at.head - at.commit)} behind head`
      : '';
  return `at ${commit}${when}${behind}`;
}

/** Whether a point-in-time read can see this commit (older servers: assume so). */
export function readable(c: Pick<Commit, 'reconstructable'>): boolean {
  return c.reconstructable !== false;
}

/** A diff's change as one N-Quads line with its sign. */
export function diffLine(q: DiffQuad): string {
  const g = q.graph ? ` ${q.graph}` : '';
  return `${q.op === '+' ? '+' : '−'} ${q.subject} ${q.predicate} ${q.object}${g} .`;
}

/** "+3 −1", or "no change". */
export function diffSummary(d: { added: number; removed: number }): string {
  if (!d.added && !d.removed) return 'no change';
  return `+${fmtInt(d.added)} −${fmtInt(d.removed)}`;
}

/**
 * The value of an expiry field: a duration (`90s`, `30m`, `12h`, `7d`, `2w`), or a
 * `datetime-local` value turned into RFC 3339. Null for an empty field; throws for an
 * unreadable one.
 */
export function expiresParam(input: string): string | null {
  const s = input.trim();
  if (!s) return null;
  if (/^\d+[smhdw]$/.test(s)) return s;
  const t = new Date(s);
  if (Number.isNaN(t.getTime())) throw new Error('Use a duration like 7d or a date and time');
  return t.toISOString();
}
