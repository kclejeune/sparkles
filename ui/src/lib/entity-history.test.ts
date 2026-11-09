import { describe, expect, it } from 'vitest';
import type { HistoryChange, HistoryOptions } from './api';
import {
  authorsOf,
  entityChanges,
  entityCommits,
  appendEntityPage,
  entityHistoryPage,
  isReadable,
  netChanges,
  readEntityHistory,
} from './entity-history';

const change = (
  commit: number,
  op: 'add' | 'remove',
  p: string,
  o: string,
  extra: Partial<HistoryChange> = {},
): HistoryChange => ({
  op,
  subject: '<urn:alice>',
  predicate: p,
  object: o,
  graph: null,
  commit,
  timestamp: `2026-10-0${commit}T12:00:00.000Z`,
  kind: 'update',
  ...extra,
});

// the acceptance dataset: commit 1 loads name and knows bob, commit 3 swaps bob for carol
const asSubject = [
  change(1, 'add', '<urn:name>', '"Alice"', { message: 'Load Alice', author: 'user:ana' }),
  change(1, 'add', '<urn:knows>', '<urn:bob>', { message: 'Load Alice', author: 'user:ana' }),
  change(3, 'remove', '<urn:knows>', '<urn:bob>', {
    message: 'Alice knows Carol',
    author: 'user:bo',
  }),
  change(3, 'add', '<urn:knows>', '<urn:carol>', {
    message: 'Alice knows Carol',
    author: 'user:bo',
  }),
];

describe('entityChanges', () => {
  it('joins the two lookups in commit order, keeping the direction', () => {
    const incoming: HistoryChange = {
      ...change(2, 'add', '<urn:knows>', '<urn:alice>'),
      subject: '<urn:dave>',
    };
    const all = entityChanges(asSubject, [incoming]);
    expect(all.map((c) => [c.commit, c.dir])).toEqual([
      [1, 'out'],
      [1, 'out'],
      [2, 'in'],
      [3, 'out'],
      [3, 'out'],
    ]);
    expect(all[2].term).toBe('<urn:dave>');
    expect(all[0].term).toBe('"Alice"');
  });
});

describe('entityCommits', () => {
  it('lists only the commits that changed the resource, with its own counts', () => {
    const commits = entityCommits(asSubject, new Map([[1, 'load']]));
    expect(commits.map((c) => c.seq)).toEqual([1, 3]);
    expect(commits[0]).toMatchObject({
      inserted: 2,
      deleted: 0,
      kind: 'load',
      message: 'Load Alice',
    });
    expect(commits[1]).toMatchObject({ inserted: 1, deleted: 1, kind: 'update' });
  });
});

describe('authorsOf', () => {
  it('maps commits to the caller that made them', () => {
    expect([...authorsOf(asSubject)]).toEqual([
      [1, 'user:ana'],
      [3, 'user:bo'],
    ]);
  });
});

describe('netChanges', () => {
  const all = entityChanges(asSubject, []);

  it('lists what commit 3 added and removed since commit 1, with who and why', () => {
    const net = netChanges(all, 1, 3);
    expect(net.map((c) => [c.op, c.term])).toEqual([
      ['remove', '<urn:bob>'],
      ['add', '<urn:carol>'],
    ]);
    expect(net.every((c) => c.commit === 3 && c.author === 'user:bo')).toBe(true);
    expect(net[0].message).toBe('Alice knows Carol');
  });

  it('is empty for no commit in between, and the order of the handles does not matter', () => {
    expect(netChanges(all, 3, 3)).toEqual([]);
    expect(netChanges(all, 3, 1)).toEqual(netChanges(all, 1, 3));
  });

  it('cancels a value that was added and removed in the range', () => {
    const flip = entityChanges(
      [
        change(2, 'add', '<urn:p>', '"x"'),
        change(4, 'remove', '<urn:p>', '"x"'),
        change(5, 'add', '<urn:p>', '"y"'),
      ],
      [],
    );
    expect(netChanges(flip, 1, 5).map((c) => c.term)).toEqual(['"y"']);
    // from before the addition to between the two, the value is still added
    expect(netChanges(flip, 1, 3).map((c) => [c.op, c.term])).toEqual([['add', '"x"']]);
  });

  it('keeps the same value apart by graph and direction', () => {
    const c = entityChanges(
      [change(2, 'add', '<urn:p>', '<urn:o>', { graph: '<urn:g1>' })],
      [{ ...change(2, 'add', '<urn:p>', '<urn:alice>'), subject: '<urn:o>' }],
    );
    expect(netChanges(c, 1, 2)).toHaveLength(2);
  });
});

describe('isReadable', () => {
  it('checks the ranges', () => {
    const r = [{ from: 5, to: 9 }];
    expect(isReadable(5, r)).toBe(true);
    expect(isReadable(10, r)).toBe(false);
    expect(isReadable(4, [])).toBe(false);
  });
});

describe('entityHistoryPage', () => {
  const ch = (commit: number, op: 'add' | 'remove', s: string, p: string, o: string) =>
    change(commit, op, p, o, { subject: s });
  const lookup = (changes: HistoryChange[], truncated = false) => ({
    dataset: 'onto',
    datasetId: 'x',
    head: 9,
    from: 0,
    to: 9,
    truncated,
    changes,
    unrecorded: [] as { from: number; to: number; reason: 'bulk' | 'before-log' | 'gap' }[],
  });

  it('joins the lookups newest first and counts a triple named twice once', () => {
    const self = ch(4, 'add', '<urn:A>', '<urn:p>', '<urn:A>');
    const page = entityHistoryPage([
      lookup([self, ch(2, 'add', '<urn:A>', '<urn:label>', '"A"')]),
      lookup([]),
      lookup([self, ch(3, 'remove', '<urn:B>', '<urn:sub>', '<urn:A>')]),
    ]);
    expect(page.commits.map((c) => [c.seq, c.added, c.removed])).toEqual([
      [4, 1, 0],
      [3, 0, 1],
      [2, 1, 0],
    ]);
    expect(page.next).toBeNull();
  });

  it('keeps only the commits above the oldest cut and reads that commit again next', () => {
    const page = entityHistoryPage([
      // cut inside commit 5
      lookup(
        [ch(7, 'add', '<urn:A>', '<urn:p>', '"x"'), ch(5, 'add', '<urn:A>', '<urn:p>', '"y"')],
        true,
      ),
      lookup([]),
      // cut inside commit 3, older than the other cut
      lookup(
        [
          ch(6, 'add', '<urn:B>', '<urn:q>', '<urn:A>'),
          ch(3, 'add', '<urn:B>', '<urn:q>', '<urn:A>'),
        ],
        true,
      ),
    ]);
    expect(page.commits.map((c) => c.seq)).toEqual([7, 6]);
    expect(page.next).toBe(5);
    expect(page.commits.every((c) => !c.partial)).toBe(true);
  });

  it('shows a commit that fills a lookup alone, partly counted, and reads below it next', () => {
    const big = [1, 2, 3].map((i) => ch(5, 'add', '<urn:A>', '<urn:p>', `"${i}"`));
    const page = entityHistoryPage([lookup(big, true), lookup([]), lookup([])]);
    expect(page.commits).toHaveLength(1);
    expect(page.commits[0]).toMatchObject({ seq: 5, added: 3, partial: true });
    expect(page.next).toBe(4);
    const first = entityHistoryPage([lookup([ch(0, 'add', '<urn:A>', '<urn:p>', '"0"')], true)]);
    expect(first.next).toBeNull();
  });

  it('lists each unrecorded range once, newest first', () => {
    const l = lookup([]);
    l.unrecorded = [
      { from: 1, to: 1, reason: 'bulk' },
      { from: 4, to: 6, reason: 'gap' },
    ];
    const page = entityHistoryPage([l, l, l]);
    expect(page.unrecorded).toEqual([
      { from: 4, to: 6, reason: 'gap' },
      { from: 1, to: 1, reason: 'bulk' },
    ]);
    expect(page.commits).toEqual([]);
  });
});

describe('readEntityHistory', () => {
  it('looks the entity up in each position of the graph, newest first, up to the cursor', async () => {
    const asked: HistoryOptions[] = [];
    await readEntityHistory(
      async (o) => {
        asked.push(o);
        return {
          dataset: 'd',
          datasetId: 'x',
          head: 3,
          from: 0,
          to: 3,
          truncated: false,
          changes: [],
          unrecorded: [],
        };
      },
      'urn:A',
      { graph: 'default', to: 7, limit: 50 },
    );
    expect(asked.map((o) => [o.subject, o.predicate, o.object])).toEqual([
      ['<urn:A>', undefined, undefined],
      [undefined, '<urn:A>', undefined],
      [undefined, undefined, '<urn:A>'],
    ]);
    expect(
      asked.every(
        (o) => o.graph === 'default' && o.order === 'desc' && o.to === '7' && o.limit === 50,
      ),
    ).toBe(true);
  });
});

describe('appendEntityPage', () => {
  const commit = (seq: number) => ({
    seq,
    timestamp: '2026-10-01T00:00:00Z',
    kind: 'update',
    added: 1,
    removed: 0,
    partial: false,
  });
  it('keeps the unrecorded ranges of earlier pages and adds each commit once', () => {
    const first = {
      commits: [commit(9)],
      next: 7,
      unrecorded: [{ from: 8, to: 8, reason: 'bulk' as const }],
    };
    const second = {
      commits: [commit(7), commit(5)],
      next: null,
      unrecorded: [{ from: 1, to: 1, reason: 'before-log' as const }],
    };
    const all = appendEntityPage(first, second);
    expect(all.commits.map((c) => c.seq)).toEqual([9, 7, 5]);
    expect(all.next).toBeNull();
    expect(all.unrecorded).toEqual([
      { from: 8, to: 8, reason: 'bulk' },
      { from: 1, to: 1, reason: 'before-log' },
    ]);
    // a page read again does not repeat its commits or ranges
    expect(appendEntityPage(all, second)).toEqual(all);
  });
});

describe('readEntityHistory pins its lookups', () => {
  it('reads every lookup up to the same commit, the head shown for the first page', async () => {
    const asked: (string | undefined)[] = [];
    await readEntityHistory(
      async (o) => {
        asked.push(o.to);
        return {
          dataset: 'd',
          datasetId: 'x',
          head: 12,
          from: 0,
          to: 9,
          truncated: false,
          changes: [],
          unrecorded: [],
        };
      },
      'urn:A',
      { graph: 'default', to: 9, limit: 50 },
    );
    expect(asked).toEqual(['9', '9', '9']);
  });
});
