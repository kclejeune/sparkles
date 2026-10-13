import { describe, expect, it } from 'vitest';
import type { HistoryChange } from './api';
import { authorsOf, entityChanges, entityCommits, isReadable, netChanges } from './entity-history';

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
