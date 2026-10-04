// The commit graph of the History panel (docs/specs/F09-branches-and-merges.md §2.9): the
// commits of several branches in time order, one lane per branch, as `git log --graph`
// draws them. Lanes, rows and edges are computed here, and the component draws them as
// SVG.
//
// Lanes. Branches form a tree, each starting from a commit of its upstream. A depth-first
// walk from `main` gives each branch a lane, with a branch's subtree to its right. Among
// the branches that start from the same upstream, the one that started latest sits
// nearest to it. A fork edge then crosses no lane that is in use at its row: every branch
// between the upstream and the forking branch started later than the fork. Merge edges
// may cross lanes. Branches without own commits get no lane. Their names show on the
// commit they started from.
//
// Rows. A row is a commit, or a segment that stands for a run of commits on one branch
// with nothing else between them in time. A segment holds commits that are neither a
// branch head, nor a merge, nor the parent of a fork or a merge, so collapsing it hides
// no edge but the branch's own line.
import type { Commit } from './api';
import { MAIN } from './branches';

/** A commit as another one names it: its branch (null once deleted), branch id and seq. */
export type CommitRef = { branch: string | null; branchId: string; seq: number };

/** A branch of `GET /$/commit-graph/{ds}`. */
export type GraphBranch = {
  name: string;
  id: string;
  ordinal: number;
  head: number;
  modified?: string;
  /** The commit it started from; null for `main`. */
  from: CommitRef | null;
  upstream: string | null;
  created?: string;
};

/** A commit of `GET /$/commit-graph/{ds}`. */
export type GraphCommit = Commit & {
  branch: string;
  branchId: string;
  /** The first parent, then a merge commit's merged commit. */
  parents: CommitRef[];
};

/** `GET /$/commit-graph/{ds}`: one page. */
export type CommitGraphPage = {
  dataset: string;
  datasetId: string;
  branches: GraphBranch[];
  /** Newest first. */
  commits: GraphCommit[];
  /** URL of the next (older) page, or null. */
  next: string | null;
};

/** The key of a commit: its branch id and seq. */
export const commitKey = (branchId: string, seq: number) => `${branchId}:${seq}`;

/** Whether branch `b` has commits of its own (beyond the commit it started from). */
export const hasOwnCommits = (b: GraphBranch) => !b.from || b.head > b.from.seq;

/**
 * The commit `r` names, on the branch that made it: a seq at or below a branch's
 * starting commit is a commit of its upstream.
 */
export function normalizeRef(r: CommitRef, byId: Map<string, GraphBranch>): CommitRef {
  let cur = r;
  for (let i = 0; i < 64; i++) {
    const b = byId.get(cur.branchId);
    if (!b?.from || cur.seq > b.from.seq) return cur;
    cur = { branch: b.from.branch, branchId: b.from.branchId, seq: cur.seq };
  }
  return cur;
}

/**
 * Lanes by branch id: `main` in lane 0, then a depth-first walk in which each branch's
 * subtree follows it, siblings that started later first. Branches whose upstream is not
 * in the list are roots after `main`'s tree, oldest first. Branches without own commits
 * get no lane.
 */
export function assignLanes(branches: GraphBranch[]): Map<string, number> {
  const byId = new Map(branches.map((b) => [b.id, b]));
  const children = new Map<string, GraphBranch[]>();
  const roots: GraphBranch[] = [];
  for (const b of branches) {
    const up = b.from ? byId.get(b.from.branchId) : undefined;
    if (!up || up.id === b.id) roots.push(b);
    else children.set(up.id, [...(children.get(up.id) ?? []), b]);
  }
  // later forks first; then the newer branch; then by name, so the order never depends
  // on the order of the input
  const byFork = (a: GraphBranch, b: GraphBranch) =>
    (b.from?.seq ?? -1) - (a.from?.seq ?? -1) ||
    b.ordinal - a.ordinal ||
    a.name.localeCompare(b.name);
  roots.sort(
    (a, b) =>
      Number(b.name === MAIN) - Number(a.name === MAIN) ||
      a.ordinal - b.ordinal ||
      a.name.localeCompare(b.name),
  );
  const lanes = new Map<string, number>();
  const seen = new Set<string>();
  const walk = (b: GraphBranch) => {
    if (seen.has(b.id)) return;
    seen.add(b.id);
    if (hasOwnCommits(b)) lanes.set(b.id, lanes.size);
    for (const c of (children.get(b.id) ?? []).sort(byFork)) walk(c);
  };
  for (const r of roots) walk(r);
  return lanes;
}

/** A row of the graph: one commit, or a collapsed run of commits on one branch. */
export type Row =
  | { kind: 'commit'; key: string; lane: number; commit: GraphCommit }
  | { kind: 'segment'; key: string; lane: number; branch: string; commits: GraphCommit[] };

/** The keys of commits that must stay visible: heads, merges, and parents of forks and merges. */
export function pinnedCommits(commits: GraphCommit[], branches: GraphBranch[]): Set<string> {
  const byId = new Map(branches.map((b) => [b.id, b]));
  const pinned = new Set<string>();
  for (const b of branches) {
    if (hasOwnCommits(b)) pinned.add(commitKey(b.id, b.head));
    else if (b.from) {
      const f = normalizeRef(b.from, byId);
      pinned.add(commitKey(f.branchId, f.seq));
    }
  }
  for (const c of commits) {
    if (c.parents.length > 1) pinned.add(commitKey(c.branchId, c.seq));
    c.parents.forEach((p, i) => {
      // a merge's source, or the commit a branch started from
      if (i > 0 || p.branchId !== c.branchId) pinned.add(commitKey(p.branchId, p.seq));
    });
  }
  return pinned;
}

/**
 * The rows of `commits` (newest first): runs of at least `minRun` commits of one branch
 * that are next to each other in time and not pinned become one segment, unless the
 * segment's key (its newest commit's) is in `expanded`. Commits of branches without a
 * lane are left out.
 */
export function buildRows(
  commits: GraphCommit[],
  branches: GraphBranch[],
  lanes: Map<string, number>,
  expanded: ReadonlySet<string> = new Set(),
  minRun = 4,
): Row[] {
  const pinned = pinnedCommits(commits, branches);
  const rows: Row[] = [];
  let run: GraphCommit[] = [];
  const flush = () => {
    if (!run.length) return;
    const first = run[0];
    const key = `seg:${commitKey(first.branchId, first.seq)}`;
    const lane = lanes.get(first.branchId) ?? 0;
    if (run.length >= minRun && !expanded.has(key))
      rows.push({ kind: 'segment', key, lane, branch: first.branch, commits: run });
    else
      for (const c of run)
        rows.push({ kind: 'commit', key: commitKey(c.branchId, c.seq), lane, commit: c });
    run = [];
  };
  for (const c of commits) {
    const lane = lanes.get(c.branchId);
    if (lane == null) continue;
    const key = commitKey(c.branchId, c.seq);
    if (pinned.has(key)) {
      flush();
      rows.push({ kind: 'commit', key, lane, commit: c });
      continue;
    }
    if (run.length && run[0].branchId !== c.branchId) flush();
    run.push(c);
  }
  flush();
  return rows;
}

/**
 * An edge between two rows. `to` is null when the parent is on a page not loaded yet, and
 * the edge runs to the bottom of the graph. `travel` is the lane the edge runs down in.
 */
export type Edge = {
  kind: 'parent' | 'fork' | 'merge';
  from: number;
  fromLane: number;
  to: number | null;
  toLane: number;
  /** the lane whose colour the edge takes */
  colour: number;
};

/** Where a parent edge ends that the graph cannot draw: the label of the missing commit. */
export type Stub = { row: number; label: string; kind: 'fork' | 'merge' };

/**
 * The edges between `rows`, and labels for parents that are not in the graph (a deleted
 * branch, one the caller cannot read, or a commit no longer kept). `more` says older pages
 * exist, so a parent on a listed branch that is not loaded yet runs off the bottom.
 */
export function layoutEdges(
  rows: Row[],
  lanes: Map<string, number>,
  more: boolean,
): { edges: Edge[]; stubs: Stub[] } {
  const rowOf = new Map<string, number>();
  rows.forEach((r, i) => {
    if (r.kind === 'commit') rowOf.set(r.key, i);
    else for (const c of r.commits) rowOf.set(commitKey(c.branchId, c.seq), i);
  });
  const edges: Edge[] = [];
  const stubs: Stub[] = [];
  const childCommits = (r: Row) => (r.kind === 'commit' ? [r.commit] : r.commits.slice(-1));
  rows.forEach((r, i) => {
    for (const c of childCommits(r)) {
      c.parents.forEach((p, n) => {
        const pl = lanes.get(p.branchId);
        const kind: Edge['kind'] = n > 0 ? 'merge' : p.branchId === c.branchId ? 'parent' : 'fork';
        const pr = rowOf.get(commitKey(p.branchId, p.seq));
        if (pr === i) return;
        if (pr != null && pl != null) {
          edges.push({
            kind,
            from: i,
            fromLane: r.lane,
            to: pr,
            toLane: pl,
            colour: kind === 'merge' ? pl : r.lane,
          });
        } else if (pl != null && more) {
          // on an older page: run down the lane it continues in
          const travel = kind === 'merge' ? pl : r.lane;
          edges.push({ kind, from: i, fromLane: r.lane, to: null, toLane: travel, colour: travel });
        } else if (kind !== 'parent') {
          stubs.push({ row: i, kind, label: `${p.branch ?? 'deleted'}@${p.seq}` });
        }
      });
    }
  });
  return { edges, stubs };
}

/** Branch names by the key of the commit they point at (their head), for labels. */
export function headLabels(branches: GraphBranch[]): Map<string, string[]> {
  const byId = new Map(branches.map((b) => [b.id, b]));
  const out = new Map<string, string[]>();
  for (const b of branches) {
    const at =
      hasOwnCommits(b) || !b.from ? { branchId: b.id, seq: b.head } : normalizeRef(b.from, byId);
    const k = commitKey(at.branchId, at.seq);
    out.set(k, [...(out.get(k) ?? []), b.name]);
  }
  return out;
}

/** Geometry of the drawing. */
export const ROW_H = 28;
export const LANE_W = 14;
export const PAD = 10;

export const laneX = (lane: number) => PAD + lane * LANE_W;
export const rowY = (row: number) => row * ROW_H + ROW_H / 2;

/**
 * The SVG path of an edge. Within a lane it is a straight line. A fork runs down the
 * child's lane and bends into the parent's lane at the parent's row. A merge bends from
 * the merge commit into the source's lane and runs down it. An edge to a page not loaded
 * runs to `bottom`.
 */
export function edgePath(e: Edge, bottom: number): string {
  const x1 = laneX(e.fromLane);
  const y1 = rowY(e.from);
  const x2 = laneX(e.toLane);
  if (e.to == null) {
    if (x1 === x2) return `M${x1} ${y1}V${bottom}`;
    const yb = y1 + ROW_H / 2;
    return `M${x1} ${y1}C${x1} ${yb} ${x2} ${y1} ${x2} ${yb}V${bottom}`;
  }
  const y2 = rowY(e.to);
  if (x1 === x2) return `M${x1} ${y1}V${y2}`;
  const r = ROW_H / 2;
  if (e.kind === 'merge') {
    // bend into the source's lane right away, then down it
    const yb = y1 + r;
    return `M${x1} ${y1}C${x1} ${yb} ${x2} ${y1} ${x2} ${yb}V${y2}`;
  }
  // down the child's lane, then bend into the parent's lane at its row
  const ya = y2 - r;
  return `M${x1} ${y1}V${ya}C${x1} ${y2} ${x2} ${ya} ${x2} ${y2}`;
}

/** The rows an edge spans, for drawing only the edges of the rows on screen. */
export const edgeSpan = (e: Edge, rows: number): [number, number] => [e.from, e.to ?? rows];

/** Merge the next page into the loaded one: the commits appended, the branches replaced. */
export function appendPage(loaded: CommitGraphPage, next: CommitGraphPage): CommitGraphPage {
  const seen = new Set(loaded.commits.map((c) => commitKey(c.branchId, c.seq)));
  return {
    ...next,
    branches: loaded.branches,
    commits: [
      ...loaded.commits,
      ...next.commits.filter((c) => !seen.has(commitKey(c.branchId, c.seq))),
    ],
  };
}

/** The visible rows of a scrolled list: `[start, end)` with `overscan` rows either side. */
export function visibleRange(
  scrollTop: number,
  height: number,
  rows: number,
  overscan = 8,
): [number, number] {
  const start = Math.max(0, Math.floor(scrollTop / ROW_H) - overscan);
  const end = Math.min(rows, Math.ceil((scrollTop + height) / ROW_H) + overscan);
  return [start, Math.max(start, end)];
}
