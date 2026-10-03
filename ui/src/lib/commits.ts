// Formatting of commits and write receipts (docs/API.md, "Commits").
import type { Commit, CommitKind, CommitPage, Receipt } from './api';
import { fmtInt } from './format';

const KIND_LABELS: Record<CommitKind, string> = {
  create: 'created',
  baseline: 'baseline',
  update: 'SPARQL update',
  'gsp-put': 'Graph Store PUT',
  'gsp-post': 'Graph Store POST',
  'gsp-delete': 'Graph Store DELETE',
  upload: 'upload',
  load: 'bulk load',
  reason: 'reasoning',
  'reason-clear': 'inferences dropped',
  transaction: 'transaction',
  embed: 'embeddings',
  unknown: 'unknown',
};

/** Human name of a commit kind ("SPARQL update", "Graph Store PUT", …). */
export function commitKindLabel(kind: string): string {
  return KIND_LABELS[kind as CommitKind] ?? kind;
}

/** Net change of a commit: "+2 −0" (a true minus sign, thousands separators). */
export function fmtDelta(c: Pick<Commit, 'inserted' | 'deleted'>): string {
  return `+${fmtInt(c.inserted)} −${fmtInt(c.deleted)}`;
}

/** "commit 42 · +2 −0" for a write that committed, or a sentence saying nothing did. */
export function receiptSummary(r: Receipt): string {
  if (!r.committed) return `No change was committed (head stays at commit ${r.commit.seq})`;
  return `commit ${r.commit.seq} · ${fmtDelta(r.commit)}`;
}

export type CommitFlag = { label: string; title: string; warn?: boolean };

/** Markers of a commit whose counts or metadata are not the usual exact kind. */
export function commitFlags(c: Commit): CommitFlag[] {
  const out: CommitFlag[] = [];
  if (c.bulk)
    out.push({
      label: 'bulk',
      title: 'Made by rebuilding the index rather than through the write-ahead log',
    });
  if (!c.exact)
    out.push({
      label: 'inexact',
      warn: true,
      title:
        'A bulk commit that also deleted quads: a quad deleted and re-added may be counted as both',
    });
  if (c.reconstructed)
    out.push({
      label: 'reconstructed',
      title: 'Rebuilt from a write-ahead log record without commit metadata',
    });
  if (c.unvalidated)
    out.push({
      label: 'unvalidated',
      warn: true,
      title: "The write skipped the dataset's write-time validation",
    });
  return out;
}

/**
 * Notes about a history that is not the whole story: commits older than `firstRetained`
 * are gone, and `complete: false` means the catalog lags the log. `oldestShown` is the
 * lowest sequence number listed so far (the retention note only applies once the list
 * reaches the oldest retained commit).
 */
export function historyNotes(
  page: Pick<CommitPage, 'firstRetained' | 'complete'>,
  oldestShown: number | null,
): string[] {
  const out: string[] = [];
  if (page.firstRetained > 0 && oldestShown != null && oldestShown <= page.firstRetained)
    out.push(
      `Commits before ${page.firstRetained} are no longer retained; history starts at commit ${page.firstRetained}.`,
    );
  if (!page.complete)
    out.push(
      'The commit catalog is behind the write-ahead log after a write error, so the newest commits may be missing.',
    );
  return out;
}

/** Append an older page to the commits listed so far: newest first, no duplicates. */
export function mergeCommits(shown: Commit[], older: Commit[]): Commit[] {
  const seen = new Set(shown.map((c) => c.seq));
  return [...shown, ...older.filter((c) => !seen.has(c.seq))].sort((a, b) => b.seq - a.seq);
}

/** The `before` parameter of the next (older) page, or null when there is none. */
export function nextBefore(page: Pick<CommitPage, 'next'>, shown: Commit[]): number | null {
  if (!page.next || !shown.length) return null;
  const m = /[?&]before=(\d+)/.exec(page.next);
  return m ? Number(m[1]) : shown[shown.length - 1].seq;
}

/** An absolute timestamp for titles and the history table ("30 Sep 2026, 16:09:45.681"). */
export function fmtCommitTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const date = d.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
  const time = d.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });
  return `${date}, ${time}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}
