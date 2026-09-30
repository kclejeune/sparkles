import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Task } from './api';
import * as b from './backups';

type Call = { url: string; init: RequestInit };

function stubFetch(respond: (url: string, init: RequestInit) => Response) {
  const calls: Call[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
    calls.push({ url, init });
    return respond(url, init);
  });
  return calls;
}

const jsonResponse = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

afterEach(() => vi.unstubAllGlobals());

const task = (state: Task['state'], progress?: number): Task => ({
  id: '7',
  kind: 'backup-create',
  dataset: 'wiki',
  state,
  startedAt: '2026-09-30T14:00:00Z',
  progress,
});

describe('backup requests', () => {
  it('encodes names and filters', async () => {
    const calls = stubFetch(() => jsonResponse({ backups: [], next: null }));
    await b.repositoryBackups('s3-main', { dataset: 'my wiki', limit: 50, policy: '' });
    expect(calls[0].url).toBe('/$/repositories/s3-main/backups?dataset=my+wiki&limit=50');
    await b.getBackup('a/b', 'local', 'x.1');
    expect(calls[1].url).toBe('/$/backups/a%2Fb/local/x.1');
  });

  it('follows the `next` cursor for all backups', async () => {
    const calls = stubFetch((url) =>
      url.includes('before=')
        ? jsonResponse({ backups: [{ name: 'old' }], next: null })
        : jsonResponse({ backups: [{ name: 'new' }], next: '2026-09-01T00:00:00Z' }),
    );
    const all = await b.allRepositoryBackups('local');
    expect(all.map((x) => x.name)).toEqual(['new', 'old']);
    expect(calls[1].url).toContain('before=2026-09-01T00%3A00%3A00Z');
  });

  it('sends restores and cancellations with the right method and body', async () => {
    const calls = stubFetch((_, init) =>
      init.method === 'DELETE'
        ? new Response(null, { status: 202 })
        : jsonResponse(task('queued'), 202),
    );
    await b.restoreBackup('wiki', 'local', 'wiki-1', { target: 'wiki', replace: true });
    expect(calls[0].url).toBe('/$/backups/wiki/local/wiki-1/restore');
    expect(calls[0].init.method).toBe('POST');
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ target: 'wiki', replace: true });
    await b.cancelTask('7');
    expect(calls[1]).toMatchObject({ url: '/$/tasks/7', init: { method: 'DELETE' } });
  });

  it('follows a task until it finishes', async () => {
    const states = [task('queued'), task('running', 0.5), task('done')];
    stubFetch(() => jsonResponse(states.shift()));
    const seen: string[] = [];
    const t = await b.followTask('7', (x) => seen.push(x.state), { intervalMs: 1 });
    expect(t.state).toBe('done');
    expect(seen).toEqual(['queued', 'running', 'done']);
  });

  it('tells full repositories from the brief view', () => {
    const brief: b.RepositoryBrief = { name: 'r', type: 'fs', readonly: true, reachable: false };
    expect(b.isFull(brief)).toBe(false);
    expect(b.reachable(brief)).toBe(false);
    expect(b.isReadonly(brief)).toBe(true);
    expect(b.isBackupTask(task('done'))).toBe(true);
    expect(b.isActive(task('queued'))).toBe(true);
    expect(b.isActive(task('cancelled'))).toBe(false);
  });
});
