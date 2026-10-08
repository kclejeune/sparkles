import { describe, expect, it } from 'vitest';
import type { Commit, Receipt } from './api';
import {
  commitFlags,
  commitKindLabel,
  fmtDelta,
  historyNotes,
  mergeCommits,
  nextBefore,
  receiptSummary,
} from './commits';

const commit = (seq: number, over: Partial<Commit> = {}): Commit => ({
  seq,
  parent: seq ? seq - 1 : null,
  ref: `commit:${seq}`,
  timestamp: '2026-09-30T16:10:15.821Z',
  kind: 'update',
  inserted: 2,
  deleted: 0,
  quads: 100 + seq,
  generation: 'gen-0002',
  bulk: false,
  exact: true,
  ...over,
});

describe('commit formatting', () => {
  it('formats the net change with a minus sign and separators', () => {
    expect(fmtDelta({ inserted: 2, deleted: 0 })).toBe('+2 −0');
    expect(fmtDelta({ inserted: 12345, deleted: 7 })).toBe('+12,345 −7');
  });

  it('names commit kinds, passing unknown ones through', () => {
    expect(commitKindLabel('gsp-put')).toBe('Graph Store PUT');
    expect(commitKindLabel('update')).toBe('SPARQL update');
    expect(commitKindLabel('something-new')).toBe('something-new');
  });

  it('summarizes receipts', () => {
    const r: Receipt = { dataset: 'ds', datasetId: 'id', committed: true, commit: commit(42) };
    expect(receiptSummary(r)).toBe('commit 42 · +2 −0');
    expect(receiptSummary({ ...r, committed: false, commit: commit(41) })).toBe(
      'No change was committed (head stays at commit 41)',
    );
  });

  it('flags bulk, inexact and reconstructed commits', () => {
    expect(commitFlags(commit(1))).toEqual([]);
    const flags = commitFlags(commit(1, { bulk: true, exact: false, reconstructed: true }));
    expect(flags.map((f) => f.label)).toEqual(['bulk', 'inexact', 'reconstructed']);
    expect(flags.find((f) => f.label === 'inexact')?.warn).toBe(true);
    const bypassed = commitFlags(commit(2, { unvalidated: true }));
    expect(bypassed.map((f) => [f.label, f.warn])).toEqual([['unvalidated', true]]);
  });

  it('does not flag a redacted commit without counts as inexact', () => {
    const { inserted, deleted, quads, exact, ...redacted } = commit(3);
    void [inserted, deleted, quads, exact];
    expect(commitFlags(redacted)).toEqual([]);
  });
});

describe('history paging', () => {
  it('merges an older page without duplicates, newest first', () => {
    const shown = [commit(5), commit(4), commit(3)];
    const merged = mergeCommits(shown, [commit(3), commit(2), commit(1)]);
    expect(merged.map((c) => c.seq)).toEqual([5, 4, 3, 2, 1]);
  });

  it('reads the next before= from the server link', () => {
    const shown = [commit(5), commit(4)];
    expect(nextBefore({ next: '/$/commits/ds?before=4&limit=2' }, shown)).toBe(4);
    expect(nextBefore({ next: null }, shown)).toBeNull();
    // a link without before= falls back to the oldest listed commit
    expect(nextBefore({ next: '/$/commits/ds?limit=2' }, shown)).toBe(4);
    expect(nextBefore({ next: '/x?before=1' }, [])).toBeNull();
  });

  it('notes truncated and incomplete histories', () => {
    expect(historyNotes({ firstRetained: 0, complete: true }, 0)).toEqual([]);
    // retention only matters once the list reaches the oldest retained commit
    expect(historyNotes({ firstRetained: 10, complete: true }, 30)).toEqual([]);
    expect(historyNotes({ firstRetained: 10, complete: true }, 10)[0]).toContain(
      'Commits before 10 are no longer retained',
    );
    expect(historyNotes({ firstRetained: 0, complete: false }, 5)[0]).toContain('behind');
  });
});
