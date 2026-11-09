// An entity's history for Explore's time slider (docs/specs/U01-time-slider.md §2.3): the
// commits that changed a resource's triples as subject or object, and the net change between
// two of them. It reads the change log's answers of `GET /{ds}/history`.
import type { Commit, HistoryChange, HistoryChanges, HistoryOptions } from './api';
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

// ---------------------------------------------------------------------------------------
// The Workbench entity frame's History view (ROADMAP item 620): the commits of a branch
// that changed triples naming an entity in the editor's graph, newest first, one bounded
// page at a time.

/** The positions a history lookup names the entity in. */
export const ENTITY_POSITIONS = ['subject', 'predicate', 'object'] as const;

/** One commit of the entity's history, with its counts of the entity's triples. */
export type EntityCommit = {
  seq: number;
  timestamp: string;
  kind: string;
  author?: string;
  message?: string;
  /** The added and removed triples that name the entity, counted once each. */
  added: number;
  removed: number;
  /** The page's lookups did not list all of this commit's triples, so the counts are low. */
  partial: boolean;
};

/** One page of the entity's history. */
export type EntityHistoryPage = {
  commits: EntityCommit[];
  /** The `to` of the next older page, or null at the end of the recorded history. */
  next: number | null;
  /** The commits of the range whose changes the change log does not hold. */
  unrecorded: HistoryChanges['unrecorded'];
};

const changeKey = (c: HistoryChange) =>
  `${c.commit}\u0000${c.op}\u0000${c.subject}\u0000${c.predicate}\u0000${c.object}\u0000${c.graph ?? ''}`;

/**
 * One page of the entity's history from its newest-first lookups as subject, predicate and
 * object, each read up to the same commit and cut at the same number of changes. A cut
 * lookup may stop inside a commit, so the page keeps only the commits newer than the
 * oldest commit at which a cut lookup stopped, and the next page reads up to that commit
 * again. When a single commit fills a lookup, the page holds that commit alone with its
 * counts marked partial, and the next page starts below it. A triple that names the entity
 * in two positions is counted once.
 */
export function entityHistoryPage(lookups: readonly HistoryChanges[]): EntityHistoryPage {
  let bound: number | null = null;
  for (const l of lookups) {
    if (!l.truncated || !l.changes.length) continue;
    const oldest = Math.min(...l.changes.map((c) => c.commit));
    if (bound == null || oldest > bound) bound = oldest;
  }
  const seen = new Set<string>();
  const by = new Map<number, EntityCommit>();
  const keep = (seq: number) => bound == null || seq > bound;
  const add = (c: HistoryChange, partial: boolean) => {
    const k = changeKey(c);
    if (seen.has(k)) return;
    seen.add(k);
    let e = by.get(c.commit);
    if (!e) {
      e = {
        seq: c.commit,
        timestamp: c.timestamp,
        kind: c.kind,
        ...(c.author ? { author: c.author } : {}),
        ...(c.message ? { message: c.message } : {}),
        added: 0,
        removed: 0,
        partial,
      };
      by.set(c.commit, e);
    }
    if (c.op === 'add') e.added++;
    else e.removed++;
  };
  for (const l of lookups) for (const c of l.changes) if (keep(c.commit)) add(c, false);
  let next: number | null = bound;
  if (bound != null && !by.size) {
    // one commit filled a lookup: show it, partly counted, and read below it next
    for (const l of lookups) for (const c of l.changes) if (c.commit === bound) add(c, true);
    next = bound > 0 ? bound - 1 : null;
  }
  const unrecorded: HistoryChanges['unrecorded'] = [];
  for (const l of lookups)
    for (const u of l.unrecorded)
      if (!unrecorded.some((x) => x.from === u.from && x.to === u.to && x.reason === u.reason))
        unrecorded.push(u);
  unrecorded.sort((a, b) => b.from - a.from);
  return { commits: [...by.values()].sort((a, b) => b.seq - a.seq), next, unrecorded };
}

/**
 * The list so far with an older page appended: each commit once, the page's cursor, and the
 * unrecorded ranges of every page read, newest first.
 */
export function appendEntityPage(
  prev: EntityHistoryPage,
  page: EntityHistoryPage,
): EntityHistoryPage {
  const commits = [...prev.commits];
  for (const c of page.commits) if (!commits.some((h) => h.seq === c.seq)) commits.push(c);
  const unrecorded = [...prev.unrecorded];
  for (const u of page.unrecorded)
    if (!unrecorded.some((x) => x.from === u.from && x.to === u.to && x.reason === u.reason))
      unrecorded.push(u);
  return {
    commits: commits.sort((a, b) => b.seq - a.seq),
    next: page.next,
    unrecorded: unrecorded.sort((a, b) => b.from - a.from),
  };
}

/**
 * Read one page of the history of `iri` on `target` in `graph` (`default` for the default
 * graph), up to commit `to`, with at most `limit` changes per lookup. Every lookup reads up
 * to the same commit, so a commit that lands meanwhile cannot be counted by some lookups
 * and not by others. The first page reads up to the head the editor shows.
 */
export async function readEntityHistory(
  read: (o: HistoryOptions) => Promise<HistoryChanges>,
  iri: string,
  o: { graph: string; to: number; limit: number; signal?: AbortSignal },
): Promise<EntityHistoryPage> {
  const term = `<${iri}>`;
  const lookups = await Promise.all(
    ENTITY_POSITIONS.map((pos) =>
      read({
        [pos]: term,
        graph: o.graph,
        order: 'desc',
        limit: o.limit,
        to: String(o.to),
        signal: o.signal,
      }),
    ),
  );
  return entityHistoryPage(lookups);
}
