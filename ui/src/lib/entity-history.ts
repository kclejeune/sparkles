// An entity's history for Explore's time slider (docs/specs/U01-time-slider.md §2.3): the
// commits that changed a resource's triples as subject or object, and the net change between
// two of them. It reads the change log's answers of `GET /{ds}/history`.
import type { Commit, HistoryChange } from './api';
import type { Range } from './timeline';

/** A change of the resource, in which direction it points, with the commit that made it. */
export type EntityChange = {
  op: 'add' | 'remove';
  /** `out`: the resource is the subject, `in`: it is the object. */
  dir: 'out' | 'in';
  predicate: string;
  /** The value of an outgoing triple, or the subject of an incoming one (N-Triples). */
  term: string;
  graph: string | null;
  commit: number;
  timestamp: string;
  author?: string;
  message?: string;
};

/** The changes of the two history lookups (the resource as subject, and as object) as one list. */
export function entityChanges(
  asSubject: readonly HistoryChange[],
  asObject: readonly HistoryChange[],
): EntityChange[] {
  const map =
    (dir: 'out' | 'in') =>
    (c: HistoryChange): EntityChange => ({
      op: c.op,
      dir,
      predicate: c.predicate,
      term: dir === 'out' ? c.object : c.subject,
      graph: c.graph,
      commit: c.commit,
      timestamp: c.timestamp,
      author: c.author,
      message: c.message,
    });
  return [...asSubject.map(map('out')), ...asObject.map(map('in'))].sort(
    (a, b) => a.commit - b.commit,
  );
}

/**
 * The commits the slider offers: one per commit that changed the resource, with the counts
 * of the resource's own additions and removals. `kind` comes from the log, and the commit
 * is readable only inside `readable` (the ranges of the dataset's catalog).
 */
export function entityCommits(
  changes: readonly (HistoryChange | EntityChange)[],
  kinds: ReadonlyMap<number, string>,
): Commit[] {
  const by = new Map<number, Commit>();
  for (const c of changes) {
    let commit = by.get(c.commit);
    if (!commit) {
      commit = {
        seq: c.commit,
        parent: null,
        ref: `commit:${c.commit}`,
        timestamp: c.timestamp,
        kind: (kinds.get(c.commit) ?? 'update') as Commit['kind'],
        inserted: 0,
        deleted: 0,
        generation: '',
        bulk: false,
        ...(c.message ? { message: c.message } : {}),
      };
      by.set(c.commit, commit);
    }
    if (c.op === 'add') commit.inserted = (commit.inserted ?? 0) + 1;
    else commit.deleted = (commit.deleted ?? 0) + 1;
  }
  return [...by.values()].sort((a, b) => a.seq - b.seq);
}

/** Who made each commit, from the log. */
export function authorsOf(
  changes: readonly { commit: number; author?: string }[],
): Map<number, string> {
  const out = new Map<number, string>();
  for (const c of changes) if (c.author) out.set(c.commit, c.author);
  return out;
}

const keyOf = (c: EntityChange) =>
  `${c.dir}\u0000${c.predicate}\u0000${c.term}\u0000${c.graph ?? ''}`;

/**
 * The net change between the state after `from` and the state after `to`: the changes of the
 * commits in `(from, to]`, with an addition and a later removal of the same value (or the
 * other way round) cancelling. Each remaining change keeps the commit, author and message of
 * the change that made it.
 */
export function netChanges(
  changes: readonly EntityChange[],
  from: number,
  to: number,
): EntityChange[] {
  const lo = Math.min(from, to);
  const hi = Math.max(from, to);
  const net = new Map<string, EntityChange>();
  for (const c of [...changes].sort((a, b) => a.commit - b.commit)) {
    if (c.commit <= lo || c.commit > hi) continue;
    const k = keyOf(c);
    const had = net.get(k);
    if (had && had.op !== c.op) net.delete(k);
    else net.set(k, c);
  }
  const out = [...net.values()];
  // removals first, as a diff lists them, then additions
  return out.sort(
    (a, b) =>
      (a.op === b.op ? 0 : a.op === 'remove' ? -1 : 1) ||
      a.predicate.localeCompare(b.predicate) ||
      a.term.localeCompare(b.term),
  );
}

/**
 * The commit number an `at` selector names when the branch's head is `head`: the head for an
 * empty selector or `head`, N for `N` and `commit:N`, and null for a time or a snapshot.
 */
export function selectorSeq(selector: string, head: number): number | null {
  const s = selector.trim();
  if (!s || s === 'head') return head;
  const m = /^(?:commit:)?(\d{1,20})$/.exec(s);
  return m ? Number(m[1]) : null;
}

/** Whether `seq` lies inside one of the ranges a read can see. */
export const isReadable = (seq: number, ranges: readonly Range[]) =>
  ranges.some((r) => seq >= r.from && seq <= r.to);
