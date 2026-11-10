// Watches on entities (ROADMAP item 619, docs/specs/U12-ontology-workbench/SPEC.md#watches):
// a user watches an entity, or an entity and its asserted subtree, in one graph of one
// branch, and the server records the commits that change it. The routes are `/$/watches`
// (docs/API.md#watches). Every answer is the caller's own watches, filtered by what the
// caller can read now, so the UI never filters for access itself.
import { json } from './api';

export type WatchScope = 'entity' | 'subtree';

export type Watch = {
  id: string;
  dataset: string;
  branch: string;
  /** absent for the default graph */
  graph?: string;
  entity: string;
  scope: WatchScope;
  /** for a subtree, the most levels below the entity that count */
  depth?: number;
  created: string;
  since: number;
  seen: number;
  unread: number;
  overflow: boolean;
  truncated: boolean;
  /** the newest matched commit */
  latest?: number;
};

/** Whether matches are also mailed to the caller, and where. */
export type WatchMail = {
  /** an email channel carries watches */
  channels: boolean;
  address?: string;
  /** why nothing is mailed, when nothing is */
  why?: string;
};

export type WatchList = { watches: Watch[]; unread: number; mail?: WatchMail };

export type WatchMatch = {
  commit: number;
  time: string;
  author?: string;
  members?: string[];
  count: number;
  summary?: boolean;
  partial?: boolean;
  exact: boolean;
  own?: boolean;
  unread: boolean;
};

export type WatchMatches = { watch: Watch; matches: WatchMatch[]; more: boolean };

export type NewWatch = {
  dataset: string;
  branch?: string;
  graph?: string | null;
  entity: string;
  scope?: WatchScope;
  depth?: number;
};

const enc = encodeURIComponent;

const query = (p: Record<string, string | number | null | undefined>) => {
  const s = new URLSearchParams();
  for (const [k, v] of Object.entries(p)) if (v != null && v !== '') s.set(k, String(v));
  const t = s.toString();
  return t ? `?${t}` : '';
};

/** The caller's watches, optionally of one dataset, entity and graph (`null`: the default graph). */
export const listWatches = (
  f: { dataset?: string; entity?: string; graph?: string | null } = {},
  signal?: AbortSignal,
) =>
  json<WatchList>(
    `/$/watches${query({
      dataset: f.dataset,
      entity: f.entity,
      graph: f.graph === null ? 'default' : f.graph,
    })}`,
    { signal },
  );

/** Watch an entity; the same watch again answers the existing one. */
export const createWatch = (w: NewWatch) =>
  json<Watch>('/$/watches', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ ...w, graph: w.graph ?? undefined }),
  });

export const deleteWatch = (id: string) =>
  json<void>(`/$/watches/${enc(id)}`, { method: 'DELETE' });

/** A page of a watch's matched commits, newest first. */
export const watchMatches = (
  id: string,
  p: { after?: number; before?: number; limit?: number } = {},
  signal?: AbortSignal,
) => json<WatchMatches>(`/$/watches/${enc(id)}/matches${query(p)}`, { signal });

/** Mark a watch's commits up to `commit` as read. */
export const markSeen = (id: string, commit: number) =>
  json<Watch>(`/$/watches/${enc(id)}/seen`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ commit }),
  });

/** How a watch reads in a list: "Entity", "Subtree" or "Subtree, 2 levels". */
export function scopeLabel(w: Pick<Watch, 'scope' | 'depth'>): string {
  if (w.scope === 'entity') return 'Entity';
  if (!w.depth) return 'Subtree';
  return `Subtree, ${w.depth} ${w.depth === 1 ? 'level' : 'levels'}`;
}

/** The watch among `ws` that covers exactly this entity, graph and branch with this scope. */
export function findWatch(
  ws: Watch[],
  t: { dataset: string; branch: string; graph: string | null; entity: string },
): Watch[] {
  return ws.filter(
    (w) =>
      w.dataset === t.dataset &&
      w.branch === t.branch &&
      (w.graph ?? null) === t.graph &&
      w.entity === t.entity,
  );
}

/** A depth typed in a field: a whole number from 1 to 1000, or none for every level. */
export function parseDepth(s: string): number | undefined | 'invalid' {
  const t = s.trim();
  if (!t) return undefined;
  if (!/^\d+$/.test(t)) return 'invalid';
  const n = Number(t);
  return n >= 1 && n <= 1000 ? n : 'invalid';
}

/** What a match lists: its members, "N changes" for a summary, or that it is unverified. */
export function matchText(m: WatchMatch): string {
  if (!m.exact) return 'Changes not listed (a bulk commit or a gap), so this may not touch it';
  if (m.summary) return `${m.count} changes under the watched entity`;
  const shown = m.members ?? [];
  const rest = m.count - shown.length;
  return rest > 0 ? `${shown.length} shown of ${m.count}` : '';
}
