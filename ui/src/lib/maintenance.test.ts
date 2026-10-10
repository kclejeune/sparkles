import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  jobOf,
  latestOf,
  nextRunText,
  outcomeText,
  retentionPlan,
  startConsolidation,
  startRetention,
} from './maintenance';
import type { IngestTask } from './review-api';

const task = (over: Partial<IngestTask> & { kind?: string } = {}): IngestTask => {
  const { kind = 'consolidation', ...rest } = over;
  return {
    id: 't1',
    dataset: 'org',
    status: 'done',
    progress: 1,
    createdAt: '2026-10-10T10:00:00Z',
    updatedAt: '2026-10-10T10:01:00Z',
    input: { kind } as IngestTask['input'],
    ...rest,
  };
};

describe('memory maintenance', () => {
  it('finds the newest task of each job', () => {
    const tasks = [
      task({ id: 'a', createdAt: '2026-10-10T09:00:00Z' }),
      task({ id: 'b', createdAt: '2026-10-10T11:00:00Z' }),
      task({ id: 'c', kind: 'retention', createdAt: '2026-10-10T12:00:00Z' }),
      { ...task({ id: 'd', createdAt: '2026-10-10T13:00:00Z' }), input: { name: 'doc.pdf' } },
    ];
    expect(jobOf(tasks[3])).toBeNull();
    expect(latestOf(tasks, 'consolidation')?.id).toBe('b');
    expect(latestOf(tasks, 'retention')?.id).toBe('c');
    expect(latestOf([], 'retention')).toBeNull();
    // a dry run changes nothing, so it is not the last run
    const dry = {
      ...task({ id: 'e', kind: 'retention', createdAt: '2026-10-10T14:00:00Z' }),
      input: { kind: 'retention', dryRun: true } as IngestTask['input'],
    };
    expect(latestOf([...tasks, dry], 'retention')?.id).toBe('c');
  });

  it('words the summary of a task list, which leaves out the counts', () => {
    const r = (result: Record<string, unknown>, kind = 'consolidation') =>
      outcomeText(task({ kind, result: result as IngestTask['result'] }));
    expect(r({ outcome: 'deleted' }, 'retention')).toBe('Deleted old session graphs');
    expect(r({ outcome: 'proposed' })).toBe('Proposed facts for review');
    expect(r({ outcome: 'nothing-to-consolidate' })).toBe('Nothing to consolidate');
  });

  it('says how a pass ended', () => {
    const r = (result: Record<string, unknown>, kind = 'consolidation') =>
      outcomeText(task({ kind, result: result as IngestTask['result'] }));
    expect(r({ outcome: 'proposed', proposed: 3, branch: 'consolidation.2026-10-10-1' })).toBe(
      'Proposed 3 facts on consolidation.2026-10-10-1 for review',
    );
    expect(r({ outcome: 'merged', proposed: 1 })).toBe('Merged 1 fact into main');
    expect(r({ outcome: 'dry-run', repeated: 4 })).toBe('Dry run: 4 repeated facts found');
    expect(r({ outcome: 'nothing-to-consolidate', scanned: 120 })).toBe(
      'Nothing to consolidate in 120 facts',
    );
    expect(r({ outcome: 'pending-review', branch: 'consolidation.x' })).toBe(
      'An earlier consolidation on consolidation.x waits for review',
    );
    expect(r({ outcome: 'deleted', deleted: [{ graph: 'g' }, { graph: 'h' }] }, 'retention')).toBe(
      'Deleted 2 session graphs',
    );
    expect(r({ outcome: 'dry-run', delete: ['g'] }, 'retention')).toBe(
      'Dry run: 1 session graph would be deleted',
    );
    expect(r({ outcome: 'nothing-to-delete' }, 'retention')).toBe(
      'No session graph is old enough to delete',
    );
    expect(
      outcomeText(task({ status: 'failed', error: { code: 'x', message: 'no agent graphs' } })),
    ).toBe('Failed: no agent graphs');
  });

  it('says when the next pass runs', () => {
    const now = Date.parse('2026-10-10T10:00:00Z');
    expect(nextRunText(undefined, now)).toBeNull();
    expect(nextRunText('due', now)).toBe('due within a minute');
    expect(nextRunText('2026-10-10T09:00:00Z', now)).toBe('due within a minute');
    expect(nextRunText('2026-10-10T10:30:00Z', now)).toBe('in 30m');
    expect(nextRunText('2026-10-10T16:00:00Z', now)).toBe('in 6h');
    expect(nextRunText('2026-10-13T10:00:00Z', now)).toBe('in 3d');
  });

  it('lists what a retention dry run would delete and keep', () => {
    const p = retentionPlan({
      after: '365d',
      graphs: [
        { graph: 'urn:s/1', ageDays: 400, delete: true },
        { graph: 'urn:s/2', ageDays: 10, kept: 'recent' },
        { graph: 'urn:s/3', kept: 'no-time' },
      ],
      delete: ['urn:s/1'],
    });
    expect(p.after).toBe('365d');
    expect(p.remove.map((g) => g.graph)).toEqual(['urn:s/1']);
    expect(p.keep.map((g) => g.kept)).toEqual(['recent', 'no-time']);
    expect(retentionPlan(undefined)).toEqual({ after: '', remove: [], keep: [] });
  });
});

describe('the maintenance routes', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('starts passes with the dataset settings, and a retention dry run', async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(task({ status: 'queued', progress: 0 })), {
        status: 202,
      });
    });
    await startConsolidation('org');
    await startRetention('org', { dryRun: true });
    expect(calls.map((c) => [c.url, c.init.method, c.init.body])).toEqual([
      ['/$/memory/org/consolidate', 'POST', '{}'],
      ['/$/memory/org/retention', 'POST', '{"dryRun":true}'],
    ]);
  });
});
