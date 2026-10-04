import { afterEach, describe, expect, it, vi } from 'vitest';
import * as api from './api';
import {
  aheadBehind,
  branchOption,
  branchParam,
  branchPath,
  cellPlace,
  deleteWarning,
  fromLabel,
  mergeChanges,
  mergeCommands,
  ntShort,
  onBranch,
  onBranchLabel,
  remainingConflicts,
  splitTarget,
  storageText,
  validBranchName,
} from './branches';

const branch = (over: Partial<api.Branch> = {}): api.Branch => ({
  name: 'dev',
  id: '9d0c41e2-0000-4000-8000-000000000000',
  ordinal: 1,
  head: 57,
  modified: '2026-10-01T10:00:00.000Z',
  from: { branch: 'main', branchId: 'd1', seq: 42 },
  upstream: 'main',
  mergeBase: { branch: 'main', seq: 42 },
  ahead: 15,
  behind: 19,
  protected: false,
  note: null,
  created: '2026-09-30T10:00:00.000Z',
  storage: { linked: true, ownBytes: 2048, heldBytes: 0 },
  ...over,
});

describe('branch names', () => {
  it('follows the snapshot name rule, with a letter, and not head', () => {
    expect(validBranchName('dev')).toBe(true);
    expect(validBranchName('release-1.2_x')).toBe(true);
    expect(validBranchName('2026-q4')).toBe(true);
    expect(validBranchName('42')).toBe(false);
    expect(validBranchName('head')).toBe(false);
    expect(validBranchName('-dev')).toBe(false);
    expect(validBranchName('a b')).toBe(false);
    expect(validBranchName('a'.repeat(65))).toBe(false);
    expect(validBranchName('')).toBe(false);
  });

  it('reads ?branch= with main as the default', () => {
    expect(branchParam('dev')).toBe('dev');
    expect(branchParam('main')).toBeNull();
    expect(branchParam('')).toBeNull();
    expect(branchParam(null)).toBeNull();
  });
});

describe('dataset targets', () => {
  it('names a branch other than main as name@branch', () => {
    expect(onBranch('prod', 'dev')).toBe('prod@dev');
    expect(onBranch('prod', 'main')).toBe('prod');
    expect(onBranch('prod', null)).toBe('prod');
    expect(splitTarget('prod@dev')).toEqual({ name: 'prod', branch: 'dev' });
    expect(splitTarget('prod')).toEqual({ name: 'prod', branch: 'main' });
  });

  it('keeps the path form on dataset routes', () => {
    expect(branchPath('/prod%40dev/sparql?send=10')).toBe('/prod@dev/sparql?send=10');
    expect(branchPath('/prod%40dev/upload')).toBe('/prod@dev/upload');
    expect(branchPath('/prod/sparql')).toBe('/prod/sparql');
  });

  it('moves the branch of an admin route into branch=', () => {
    expect(branchPath('/$/stats/prod%40dev')).toBe('/$/stats/prod?branch=dev');
    expect(branchPath('/$/stats/prod%40dev?at=commit%3A3')).toBe(
      '/$/stats/prod?at=commit%3A3&branch=dev',
    );
    expect(branchPath('/$/commits/prod%40dev?limit=20')).toBe(
      '/$/commits/prod?limit=20&branch=dev',
    );
    expect(branchPath('/$/snapshots/prod%40dev/v1')).toBe('/$/snapshots/prod/v1?branch=dev');
    expect(branchPath('/$/cache/clear/prod%40dev')).toBe('/$/cache/clear/prod?branch=dev');
    expect(branchPath('/$/datasets/prod%40dev/clone')).toBe('/$/datasets/prod/clone?branch=dev');
    expect(branchPath('/$/vector/prod%40dev/emb/recall')).toBe(
      '/$/vector/prod/emb/recall?branch=dev',
    );
  });

  it('leaves main, and the routes without branches, alone', () => {
    expect(branchPath('/$/stats/prod')).toBe('/$/stats/prod');
    expect(branchPath('/$/stats/prod%40main')).toBe('/$/stats/prod');
    // the server refuses a branch here, so the target is passed through as a name
    expect(branchPath('/$/quota/prod%40dev')).toBe('/$/quota/prod%40dev');
    expect(branchPath('/$/datasets/prod%40dev')).toBe('/$/datasets/prod%40dev');
    expect(branchPath('/$/tasks')).toBe('/$/tasks');
  });
});

describe('branch text', () => {
  it('describes where a branch started and how far it is from its upstream', () => {
    expect(fromLabel(branch())).toBe('main@42');
    expect(fromLabel(branch({ from: { branch: null, branchId: 'x', seq: 7 } }))).toBe('deleted@7');
    expect(fromLabel(branch({ from: null }))).toBe('');
    expect(aheadBehind(branch())).toBe('15 ahead, 19 behind');
    expect(aheadBehind(branch({ ahead: 0, behind: 0 }))).toBe('up to date');
    expect(aheadBehind(branch({ upstream: null }))).toBe('');
  });

  it('describes the disk it holds and labels it in the selector', () => {
    expect(storageText(branch())).toBe('2.0 KiB own');
    expect(storageText(branch({ storage: { linked: false, ownBytes: 0, heldBytes: 4096 } }))).toBe(
      '0 B own, 4.0 KiB held',
    );
    expect(branchOption(branch())).toBe('dev · commit 57');
  });

  it('names the branch of a result other than main', () => {
    expect(onBranchLabel('dev', 'at commit 57')).toBe('dev · at commit 57');
    expect(onBranchLabel('main', 'commit 57')).toBe('commit 57');
    expect(onBranchLabel(null, 'commit 57')).toBe('commit 57');
  });

  it('warns before deleting unmerged commits', () => {
    expect(deleteWarning(branch({ ahead: 0 }))).toBeNull();
    expect(deleteWarning(branch({ ahead: 1 }))).toBe(
      'dev has 1 commit that main does not have. Deleting it loses them.',
    );
  });
});

describe('merges', () => {
  const preview: api.MergeOutcome = {
    merged: false,
    upToDate: false,
    fastForward: false,
    source: { branch: 'dev', seq: 57 },
    target: { branch: 'main', seq: 61 },
    base: { branch: 'main', seq: 42 },
    changes: { inserted: 3, deleted: 1 },
    conflicts: { found: 0, resolved: 0 },
  };

  it('counts the conflicts left in a preview and in a conflict report', () => {
    expect(remainingConflicts(preview)).toBe(0);
    expect(remainingConflicts({ ...preview, conflictCount: 2 })).toBe(2);
    expect(remainingConflicts({ ...preview, conflicts: 3 })).toBe(3);
    expect(mergeChanges(preview)).toBe('+3 −1');
  });

  it('shortens the terms of a conflict with the prefixes', () => {
    const p = { xsd: 'http://www.w3.org/2001/XMLSchema#', ex: 'http://ex.org/' };
    expect(ntShort('"31"^^<http://www.w3.org/2001/XMLSchema#integer>', p)).toBe(
      '"31"^^xsd:integer',
    );
    expect(ntShort('<http://ex.org/age>', p)).toBe('ex:age');
    expect(ntShort('"a <http://ex.org/x> b"', p)).toBe('"a <http://ex.org/x> b"');
    expect(ntShort('<urn:x>', p)).toBe('<urn:x>');
    expect(
      cellPlace(
        {
          graph: '<http://ex.org/g>',
          subject: '<http://ex.org/a>',
          base: [],
          ours: [],
          theirs: [],
        },
        p,
      ),
    ).toBe('ex:a in ex:g');
  });

  it('places a conflict and gives the command that resolves it', () => {
    expect(
      cellPlace({
        graph: null,
        subject: '<http://ex.org/a>',
        predicate: '<http://ex.org/age>',
        base: [],
        ours: [],
        theirs: [],
      }),
    ).toBe('<http://ex.org/a> <http://ex.org/age> (default graph)');
    expect(
      cellPlace({ graph: '<urn:g>', subject: '<urn:s>', base: [], ours: [], theirs: [] }),
    ).toBe('<urn:s> in <urn:g>');
    expect(
      mergeCommands({
        server: 'http://localhost:3030',
        dataset: 'prod',
        source: 'dev',
        target: 'main',
      }),
    ).toEqual([
      'sparkles merge --server http://localhost:3030 --dataset prod dev --into main --on-conflict theirs',
      'sparkles merge --server http://localhost:3030 --dataset prod dev --into main --resolve FILE.json',
    ]);
  });
});

describe('requests on a branch', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('sends the branch with each kind of request', async () => {
    const urls: string[] = [];
    vi.stubGlobal('fetch', async (url: string) => {
      urls.push(url);
      return new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } });
    });
    const ds = onBranch('prod', 'dev');
    await api.datasetStats(ds);
    await api.commits(ds, { limit: 5 });
    await api.update(ds, 'INSERT DATA { <urn:a> <urn:b> 1 }');
    await api.branches('prod');
    expect(urls).toEqual([
      '/$/stats/prod?branch=dev',
      '/$/commits/prod?limit=5&branch=dev',
      '/prod@dev/update?receipt=true',
      '/$/branches/prod',
    ]);
  });

  it('keeps the conflict report of a refused merge', async () => {
    const report = {
      error: '1 conflict merging dev (commit 57) into main (commit 61)',
      code: 'merge-conflict',
      conflicts: 1,
      cells: [{ graph: null, subject: '<urn:s>', base: [], ours: ['1'], theirs: ['2'] }],
    };
    vi.stubGlobal(
      'fetch',
      async () =>
        new Response(JSON.stringify(report), {
          status: 409,
          headers: { 'Content-Type': 'application/json' },
        }),
    );
    const e = await api.merge('prod', { source: 'dev' }).catch((x) => x);
    expect(e).toBeInstanceOf(api.ApiError);
    expect(e.code).toBe('merge-conflict');
    expect(e.body?.cells).toHaveLength(1);
  });
});
