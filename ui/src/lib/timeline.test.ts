import { describe, expect, it } from 'vitest';
import type { Commit } from './api';
import {
  bucketize,
  buildTrack,
  caption,
  first,
  fullView,
  goneRanges,
  goneTitle,
  markGone,
  playDelay,
  playNext,
  positions,
  selectorOf,
  seqOfSelector,
  snapshotStep,
  step,
  stopAt,
  timeFailure,
  valueText,
  zoomView,
} from './timeline';

const T0 = Date.parse('2026-10-01T12:00:00.000Z');

const commit = (seq: number, extra: Partial<Commit> = {}): Commit => ({
  seq,
  parent: seq ? seq - 1 : null,
  ref: `commit:${seq}`,
  timestamp: new Date(T0 + seq * 60_000).toISOString(),
  kind: 'update',
  inserted: 1,
  deleted: 0,
  quads: seq,
  generation: 'gen-0001',
  bulk: false,
  ...extra,
});

const range = (n: number, from = 1) => Array.from({ length: n }, (_, i) => commit(from + i));

describe('buildTrack', () => {
  it('sorts the commits and marks the head', () => {
    const t = buildTrack([commit(3), commit(1), commit(2)]);
    expect(t.stops.map((s) => s.seq)).toEqual([1, 2, 3]);
    expect(t.head).toBe(3);
    expect(t.stops.map((s) => s.head)).toEqual([false, false, true]);
    expect(t.gone).toEqual([]);
    expect(t.oldestReadable).toBe(1);
  });

  it('adds a head stop when no commit of the track is the head', () => {
    const t = buildTrack([commit(1), commit(3)], { head: 5 });
    expect(t.stops.map((s) => s.seq)).toEqual([1, 3, 5]);
    expect(t.stops[2]).toMatchObject({ head: true, readable: true, kind: 'head' });
  });

  it('reads the unreadable commits from the catalog and finds the gone stretches', () => {
    const t = buildTrack([
      commit(1, { reconstructable: false }),
      commit(2, { reconstructable: false }),
      commit(3),
      commit(4, { reconstructable: false }),
      commit(5),
    ]);
    expect(t.gone).toEqual([
      { from: 1, to: 2 },
      { from: 4, to: 4 },
    ]);
    expect(t.oldestReadable).toBe(3);
    expect(first(t)).toBe(3);
  });

  it('takes readable ranges over the commits own flag', () => {
    const t = buildTrack(range(6), { readable: [{ from: 4, to: 6 }] });
    expect(t.gone).toEqual([{ from: 1, to: 3 }]);
  });

  it('marks snapshots, merges and the fork point, and takes authors', () => {
    const t = buildTrack(
      [
        commit(1),
        commit(2, { snapshots: ['s2'] }),
        commit(3, { kind: 'merge', mergedFrom: { branch: 'dev', branchId: 'x', seq: 9 } }),
      ],
      { fork: 1, authors: new Map([[2, 'ana']]) },
    );
    expect(t.stops[0].fork).toBe(true);
    expect(t.stops[1]).toMatchObject({ snapshots: ['s2'], author: 'ana' });
    expect(t.stops[2].merge).toBe(true);
  });

  it('builds an empty track', () => {
    const t = buildTrack([]);
    expect(t.stops).toHaveLength(1);
    expect(t.stops[0].head).toBe(true);
  });
});

describe('markGone', () => {
  it('rules out the failed stop when the error has no ranges', () => {
    const t = markGone(buildTrack(range(4)), 1);
    expect(t.gone).toEqual([{ from: 1, to: 1 }]);
    expect(t.oldestReadable).toBe(2);
  });

  it('applies the readable ranges of the error body, never to the head', () => {
    const t = markGone(buildTrack(range(6)), 1, [{ from: 3, to: 5 }]);
    expect(t.gone).toEqual([{ from: 1, to: 2 }]);
    expect(t.stops[5].readable).toBe(true);
  });
});

describe('selectors', () => {
  const t = buildTrack([commit(1), commit(2, { snapshots: ['s2'] }), commit(3)]);

  it('writes the head as empty and a commit as commit:N', () => {
    expect(selectorOf(t, 3)).toBe('');
    expect(selectorOf(t, 2)).toBe('commit:2');
  });

  it('reads selectors back', () => {
    expect(seqOfSelector(t, '')).toBe(3);
    expect(seqOfSelector(t, 'head')).toBe(3);
    expect(seqOfSelector(t, 'commit:1')).toBe(1);
    expect(seqOfSelector(t, '2')).toBe(2);
    expect(seqOfSelector(t, 'snapshot:s2')).toBe(2);
    expect(seqOfSelector(t, 'snapshot:nope')).toBeNull();
    expect(seqOfSelector(t, 'commit:9')).toBeNull();
    expect(seqOfSelector(t, new Date(T0 + 90_000).toISOString().replace(/^/, 'time:'))).toBe(1);
    expect(seqOfSelector(t, 'time:garbage')).toBeNull();
  });
});

describe('step', () => {
  const t = buildTrack(range(30), { readable: [{ from: 5, to: 30 }] });

  it('moves one commit and stops at the ends', () => {
    expect(step(t, 10, 1)).toBe(11);
    expect(step(t, 10, -1)).toBe(9);
    expect(step(t, 30, 1)).toBe(30);
    expect(step(t, 5, -1)).toBe(5);
  });

  it('skips commits that cannot be read', () => {
    expect(step(t, 6, -1, 10)).toBe(5);
  });

  it('moves ten at a time', () => {
    expect(step(t, 12, 1, 10)).toBe(22);
    expect(step(t, 25, 1, 10)).toBe(30);
  });

  it('starts from the nearest readable commit when the position is between stops', () => {
    const sparse = buildTrack([commit(1), commit(5), commit(9)]);
    expect(step(sparse, 3, 1)).toBe(5);
    expect(step(sparse, 3, -1)).toBe(1);
  });
});

describe('snapshotStep', () => {
  const t = buildTrack([
    commit(1),
    commit(2, { snapshots: ['a'] }),
    commit(3),
    commit(4, { snapshots: ['b'] }),
    commit(5),
  ]);
  it('jumps to the next and previous named snapshot', () => {
    expect(snapshotStep(t, 1, 1)).toBe(2);
    expect(snapshotStep(t, 2, 1)).toBe(4);
    expect(snapshotStep(t, 5, -1)).toBe(4);
    expect(snapshotStep(t, 4, -1)).toBe(2);
    expect(snapshotStep(t, 4, 1)).toBe(4);
  });
});

describe('playNext', () => {
  it('visits the next stop and stops at the head', () => {
    const t = buildTrack(range(3));
    expect(playNext(t, 1)).toBe(2);
    expect(playNext(t, 2)).toBe(3);
    expect(playNext(t, 3)).toBeNull();
  });

  it('covers several stops per step and clamps to the head', () => {
    const t = buildTrack(range(10));
    expect(playNext(t, 1, 4)).toBe(5);
    expect(playNext(t, 8, 4)).toBe(10);
  });

  it('stops in front of a stretch that cannot be read', () => {
    const t = buildTrack([commit(1), commit(2, { reconstructable: false }), commit(3)]);
    expect(playNext(t, 1)).toBeNull();
  });
});

describe('playDelay', () => {
  it('waits the speeds interval, or 1.5 times the last read when that is longer', () => {
    expect(playDelay(1, 100)).toBe(1000);
    expect(playDelay(2, 100)).toBe(500);
    expect(playDelay(4, 100)).toBe(250);
    expect(playDelay(4, 400)).toBe(600);
  });
});

describe('positions and stopAt', () => {
  const t = buildTrack([commit(1), commit(2), commit(10)]);
  const v = fullView(t);

  it('spaces stops by sequence number', () => {
    const p = positions(t, v, 'sequence', 128, 14);
    expect(p.get(1)).toBe(14);
    expect(p.get(10)).toBe(114);
    expect(p.get(2)).toBeCloseTo(14 + 100 / 9, 5);
  });

  it('spaces stops by time', () => {
    const uneven = buildTrack([
      commit(1),
      commit(2, { timestamp: new Date(T0 + 60_000 * 9).toISOString() }),
      commit(3, { timestamp: new Date(T0 + 60_000 * 10).toISOString() }),
    ]);
    const p = positions(uneven, fullView(uneven), 'time', 128, 14);
    expect(p.get(2)).toBeCloseTo(14 + (100 * 8) / 9, 5);
  });

  it('puts a single stop in the middle', () => {
    const one = buildTrack([commit(1)]);
    expect(positions(one, fullView(one), 'sequence', 100, 10).get(1)).toBe(50);
  });

  it('finds the nearest stop to a pixel', () => {
    expect(stopAt(t, v, 'sequence', 128, 0)).toBe(1);
    expect(stopAt(t, v, 'sequence', 128, 30)).toBe(2);
    expect(stopAt(t, v, 'sequence', 128, 200)).toBe(10);
  });

  it('gives way to the nearest readable stop', () => {
    const gone = buildTrack([commit(1, { reconstructable: false }), commit(2), commit(3)]);
    expect(stopAt(gone, fullView(gone), 'sequence', 128, 0)).toBe(2);
    expect(stopAt(gone, fullView(gone), 'sequence', 128, 0, false)).toBe(1);
  });
});

describe('zoomView', () => {
  const t = buildTrack(range(100));
  it('narrows around the centre and widens back', () => {
    const v = zoomView(t, fullView(t), 4, 50);
    expect(v.to - v.from + 1).toBe(25);
    expect(v.from).toBeLessThanOrEqual(50);
    expect(v.to).toBeGreaterThanOrEqual(50);
    const back = zoomView(t, v, 1 / 4, 50);
    expect(back).toEqual(fullView(t));
  });

  it('stays inside the track', () => {
    const v = zoomView(t, fullView(t), 20, 0);
    expect(v.from).toBe(0);
    const w = zoomView(t, fullView(t), 20, 99);
    expect(w.to).toBe(99);
  });

  it('shows a short track whole', () => {
    const s = buildTrack(range(5));
    expect(zoomView(s, fullView(s), 4, 2)).toEqual(fullView(s));
  });
});

describe('bucketize', () => {
  it('gives one bucket per commit when there is room', () => {
    const t = buildTrack(range(10));
    const b = bucketize(t, fullView(t), 'sequence', 400);
    expect(b).toHaveLength(10);
    expect(b.every((x) => x.commits === 1)).toBe(true);
  });

  it('groups commits when the track is short of pixels and sums their changes', () => {
    const t = buildTrack(range(100).map((c) => ({ ...c, inserted: 2, deleted: 1 })));
    const b = bucketize(t, fullView(t), 'sequence', 200, 7);
    expect(b.length).toBeLessThan(100);
    expect(b.reduce((n, x) => n + x.commits, 0)).toBe(100);
    expect(b.reduce((n, x) => n + x.inserted, 0)).toBe(200);
    expect(b.reduce((n, x) => n + x.deleted, 0)).toBe(100);
    expect(b[0].first).toBe(1);
    expect(b.at(-1)!.last).toBe(100);
    for (let i = 1; i < b.length; i++) expect(b[i].first).toBe(b[i - 1].last + 1);
  });

  it('carries snapshots, merges and unreadable commits into a bucket', () => {
    const t = buildTrack([
      commit(1, { reconstructable: false }),
      commit(2, { snapshots: ['s'] }),
      commit(3, { kind: 'merge' }),
    ]);
    const [b] = bucketize(t, fullView(t), 'sequence', 20, 100);
    expect(b).toMatchObject({ commits: 3, snapshots: ['s'], merges: 1, readable: false });
  });

  it('does not count the synthetic head stop as a commit', () => {
    const t = buildTrack(range(2), { head: 5 });
    const b = bucketize(t, fullView(t), 'sequence', 400);
    expect(b.reduce((n, x) => n + x.commits, 0)).toBe(2);
  });
});

describe('text', () => {
  it('captions a stop', () => {
    const t = buildTrack([commit(42, { message: 'fix titles', inserted: 3, deleted: 1 })], {
      authors: new Map([[42, 'ana']]),
    });
    expect(caption(t.stops[0])).toBe('42 · SPARQL update by ana · “fix titles” · +3 −1');
  });

  it('writes the value text for screen readers', () => {
    const t = buildTrack([commit(42, { inserted: 3, deleted: 1 })], {
      authors: new Map([[42, 'ana']]),
    });
    const text = valueText(t.stops[0]);
    expect(text).toMatch(/^commit 42, .*2026.*, SPARQL update by ana, head, 3 added, 1 removed$/);
  });

  it('explains a hatched stretch', () => {
    const t = buildTrack([commit(1, { reconstructable: false }), commit(2)]);
    expect(goneTitle(t, t.gone[0])).toBe(
      'Commit 1 can no longer be read. The oldest commit that can be read is 2.',
    );
    expect(goneRanges(t.stops)).toEqual(t.gone);
  });
});

describe('timeFailure', () => {
  it('reads a history-gone body', () => {
    const f = timeFailure({
      status: 410,
      code: 'history-gone',
      body: {
        code: 'history-gone',
        commit: 1,
        oldestReconstructable: 40,
        reconstructable: [{ from: 40, to: 57 }],
      },
    });
    expect(f).toEqual({
      seq: 1,
      readable: [{ from: 40, to: 57 }],
      title: 'Commit 1 can no longer be read',
      detail: 'Readable commits: 40–57.',
    });
  });

  it('falls back to the oldest readable commit', () => {
    const f = timeFailure({
      status: 410,
      body: { code: 'history-gone', commit: 3, oldestReconstructable: 9 },
    });
    expect(f?.detail).toBe('The oldest readable commit is 9.');
  });

  it('reads the history cache budget', () => {
    const f = timeFailure({ status: 507, body: { budget: 'history-memory', limit: 1073741824 } });
    expect(f?.title).toMatch(/history cache/);
    expect(f?.detail).toBe('The limit is 1.0 GiB.');
  });

  it('leaves other errors to the view', () => {
    expect(timeFailure({ status: 400, body: {} })).toBeNull();
    expect(timeFailure(new Error('x'))).toBeNull();
    expect(timeFailure(null)).toBeNull();
  });
});
