import { describe, expect, it } from 'vitest';
import {
  appendPage,
  assignLanes,
  buildRows,
  commitKey,
  edgePath,
  headLabels,
  layoutEdges,
  normalizeRef,
  pinnedCommits,
  ROW_H,
  visibleRange,
  type CommitGraphPage,
  type GraphBranch,
  type GraphCommit,
} from './commit-graph';

const branch = (
  name: string,
  ordinal: number,
  head: number,
  from: [string, number] | null = null,
): GraphBranch => ({
  name,
  id: `id-${name}`,
  ordinal,
  head,
  from: from ? { branch: from[0], branchId: `id-${from[0]}`, seq: from[1] } : null,
  upstream: from ? from[0] : null,
});

let clock = 0;
/** A commit of `b`, its parents given as `[branch, seq]`. */
const commit = (b: string, seq: number, ...parents: [string, number][]): GraphCommit => ({
  seq,
  parent: seq ? seq - 1 : null,
  ref: `commit:${seq}`,
  timestamp: new Date(1_700_000_000_000 + clock++ * 1000).toISOString(),
  kind: parents.length > 1 ? 'merge' : 'update',
  inserted: 1,
  deleted: 0,
  quads: seq,
  generation: 'gen-0001',
  bulk: false,
  exact: true,
  branch: b,
  branchId: `id-${b}`,
  parents: parents.map(([pb, ps]) => ({ branch: pb, branchId: `id-${pb}`, seq: ps })),
});

describe('assignLanes', () => {
  it('puts main first and each subtree to the right of its upstream, later forks nearer', () => {
    const lanes = assignLanes([
      branch('main', 0, 20),
      branch('early', 1, 9, ['main', 5]),
      branch('late', 2, 14, ['main', 10]),
      branch('fix', 3, 16, ['late', 12]),
    ]);
    expect([...lanes]).toEqual([
      ['id-main', 0],
      ['id-late', 1],
      ['id-fix', 2],
      ['id-early', 3],
    ]);
  });

  it('does not depend on the order of the branches', () => {
    const list = [
      branch('main', 0, 20),
      branch('a', 1, 9, ['main', 5]),
      branch('b', 2, 9, ['main', 5]),
      branch('c', 3, 12, ['a', 7]),
    ];
    const one = [...assignLanes(list)];
    const two = [...assignLanes([...list].reverse())];
    expect(two).toEqual(one);
    // same fork point: the newer branch nearer to main
    expect(one.map(([id]) => id)).toEqual(['id-main', 'id-b', 'id-a', 'id-c']);
  });

  it('gives no lane to a branch without own commits, and roots an orphan after main', () => {
    const lanes = assignLanes([
      branch('main', 0, 4),
      branch('empty', 1, 4, ['main', 4]),
      // its upstream is not listed (the caller cannot read it)
      branch('orphan', 2, 7, ['hidden', 3]),
    ]);
    expect([...lanes]).toEqual([
      ['id-main', 0],
      ['id-orphan', 1],
    ]);
  });

  it('keeps lanes stable with about 20 branches', () => {
    const list = [branch('main', 0, 100)];
    for (let i = 1; i <= 20; i++) list.push(branch(`b${i}`, i, 100 + i, ['main', i * 3]));
    const lanes = assignLanes(list);
    expect(lanes.size).toBe(21);
    expect(new Set(lanes.values()).size).toBe(21);
    // the latest fork is next to main
    expect(lanes.get('id-b20')).toBe(1);
    expect(lanes.get('id-b1')).toBe(20);
  });
});

describe('normalizeRef', () => {
  it('follows a seq at or below a start into the upstream', () => {
    const byId = new Map(
      [branch('main', 0, 9), branch('dev', 1, 9, ['main', 5]), branch('fix', 2, 9, ['dev', 7])].map(
        (b) => [b.id, b],
      ),
    );
    expect(normalizeRef({ branch: 'fix', branchId: 'id-fix', seq: 4 }, byId).branchId).toBe(
      'id-main',
    );
    expect(normalizeRef({ branch: 'fix', branchId: 'id-fix', seq: 6 }, byId).branchId).toBe(
      'id-dev',
    );
    expect(normalizeRef({ branch: 'fix', branchId: 'id-fix', seq: 8 }, byId).branchId).toBe(
      'id-fix',
    );
  });
});

/** main 1–8 with a merge of dev at 8; dev forked at 2 with 3–5. Newest first. */
function history() {
  clock = 0;
  const branches = [branch('main', 0, 8), branch('dev', 1, 5, ['main', 2])];
  const asc = [
    commit('main', 1, ['main', 0]),
    commit('main', 2, ['main', 1]),
    commit('dev', 3, ['main', 2]),
    commit('dev', 4, ['dev', 3]),
    commit('dev', 5, ['dev', 4]),
    commit('main', 3, ['main', 2]),
    commit('main', 4, ['main', 3]),
    commit('main', 5, ['main', 4]),
    commit('main', 6, ['main', 5]),
    commit('main', 7, ['main', 6]),
    commit('main', 8, ['main', 7], ['dev', 5]),
  ];
  return { branches, commits: asc.reverse() };
}

describe('rows and edges', () => {
  it('pins heads, merges and the parents of forks and merges', () => {
    const { branches, commits } = history();
    const pinned = pinnedCommits(commits, branches);
    expect([...pinned].sort()).toEqual(['id-main:8', 'id-dev:5', 'id-main:2'].sort());
  });

  it('collapses a long run of one branch into a segment, unless expanded', () => {
    const { branches, commits } = history();
    const lanes = assignLanes(branches);
    const rows = buildRows(commits, branches, lanes, new Set(), 4);
    const kinds = rows.map((r) => (r.kind === 'segment' ? `seg×${r.commits.length}` : r.key));
    // main 3–7 is a run of five; dev 3–4 is too short
    expect(kinds).toEqual([
      'id-main:8',
      'seg×5',
      'id-dev:5',
      'id-dev:4',
      'id-dev:3',
      'id-main:2',
      'id-main:1',
    ]);
    const seg = rows[1];
    const open = buildRows(commits, branches, lanes, new Set([seg.key]), 4);
    expect(open).toHaveLength(11);
  });

  it('draws parent, fork and merge edges, and runs edges to older pages off the bottom', () => {
    const { branches, commits } = history();
    const lanes = assignLanes(branches);
    const rows = buildRows(commits, branches, lanes, new Set(), 4);
    const { edges, stubs } = layoutEdges(rows, lanes, true);
    const merge = edges.find((e) => e.kind === 'merge')!;
    expect(merge).toMatchObject({ from: 0, to: 2, fromLane: 0, toLane: 1, colour: 1 });
    const fork = edges.find((e) => e.kind === 'fork')!;
    expect(fork).toMatchObject({ from: 4, to: 5, fromLane: 1, toLane: 0 });
    // main 1's parent 0 is on a page not loaded yet
    expect(edges.find((e) => e.from === 6)).toMatchObject({ to: null, toLane: 0 });
    expect(stubs).toEqual([]);
    // without more pages, nothing runs off the bottom
    expect(layoutEdges(rows, lanes, false).edges.some((e) => e.to == null)).toBe(false);
  });

  it('labels parents that are not in the graph', () => {
    clock = 0;
    const branches = [branch('main', 0, 2)];
    const commits = [commit('main', 2, ['main', 1], ['gone', 7]), commit('main', 1, ['main', 0])];
    commits[0].parents[1].branch = null;
    const lanes = assignLanes(branches);
    const rows = buildRows(commits, branches, lanes);
    const { stubs } = layoutEdges(rows, lanes, false);
    expect(stubs).toEqual([{ row: 0, kind: 'merge', label: 'deleted@7' }]);
  });

  it('names branches at their heads, and a branch without commits at its start', () => {
    const labels = headLabels([
      branch('main', 0, 8),
      branch('dev', 1, 5, ['main', 2]),
      branch('idle', 2, 2, ['main', 2]),
    ]);
    expect(labels.get(commitKey('id-main', 8))).toEqual(['main']);
    expect(labels.get(commitKey('id-dev', 5))).toEqual(['dev']);
    expect(labels.get(commitKey('id-main', 2))).toEqual(['idle']);
  });
});

describe('geometry and paging', () => {
  it('draws straight lines within a lane and bends between lanes', () => {
    expect(
      edgePath({ kind: 'parent', from: 0, fromLane: 0, to: 2, toLane: 0, colour: 0 }, 999),
    ).toBe(`M10 ${ROW_H / 2}V${2 * ROW_H + ROW_H / 2}`);
    const fork = edgePath({ kind: 'fork', from: 0, fromLane: 1, to: 3, toLane: 0, colour: 1 }, 999);
    expect(fork).toMatch(/^M24 14V84C24 98 10 84 10 98$/);
    expect(
      edgePath({ kind: 'parent', from: 1, fromLane: 2, to: null, toLane: 2, colour: 2 }, 500),
    ).toBe('M38 42V500');
  });

  it('renders only the rows in view', () => {
    expect(visibleRange(0, 280, 5000, 0)).toEqual([0, 10]);
    expect(visibleRange(ROW_H * 1000, 280, 5000, 8)).toEqual([992, 1018]);
    expect(visibleRange(ROW_H * 4995, 280, 5000, 8)).toEqual([4987, 5000]);
  });

  it('appends a page without repeating commits', () => {
    const { branches, commits } = history();
    const first: CommitGraphPage = {
      dataset: 'd',
      datasetId: 'x',
      branches,
      commits: commits.slice(0, 6),
      next: '/next',
    };
    const second: CommitGraphPage = { ...first, commits: commits.slice(5), next: null };
    const all = appendPage(first, second);
    expect(all.commits).toHaveLength(commits.length);
    expect(all.next).toBeNull();
  });
});
