// The command palette's recent picks (ROADMAP item 631), kept per viewer in localStorage.
// Storage can be blocked or throw, so every read and write goes through lib/storage.ts and
// the palette works without it, only with no recent items.

import type { RecentEntry } from './launcher.svelte';
import { load, save } from './storage';

export const RECENT_KEY = 'sparkles.launcher.recent';
/** How many picks are kept; the palette shows the first few that still apply. */
export const RECENT_MAX = 20;

const valid = (e: unknown): e is RecentEntry => {
  const r = e as Partial<RecentEntry> | null;
  return (
    !!r &&
    typeof r.provider === 'string' &&
    typeof r.id === 'string' &&
    typeof r.title === 'string' &&
    typeof r.at === 'number'
  );
};

/** The remembered picks, newest first; empty when storage is missing, blocked or corrupt. */
export function loadRecent(): RecentEntry[] {
  const raw = load<unknown>(RECENT_KEY, []);
  return Array.isArray(raw) ? raw.filter(valid).slice(0, RECENT_MAX) : [];
}

/** Whether two picks are of the same item: the same provider, id and dataset. */
const sameItem = (a: RecentEntry, b: RecentEntry) =>
  a.provider === b.provider && a.id === b.id && a.ds === b.ds;

/**
 * `list` with `entry` first and an earlier pick of the same item dropped. A pick that
 * belongs to a dataset is the same item only on the same dataset, so a pick on one dataset
 * keeps the pick of the same IRI or name on another; a pick of no dataset has `ds` null.
 */
export function addRecent(list: readonly RecentEntry[], entry: RecentEntry): RecentEntry[] {
  return [entry, ...list.filter((e) => !sameItem(e, entry))].slice(0, RECENT_MAX);
}

/** The picks of both lists, each item once with its latest pick, newest first. */
export function mergeRecent(a: readonly RecentEntry[], b: readonly RecentEntry[]): RecentEntry[] {
  const out: RecentEntry[] = [];
  for (const e of [...a, ...b].sort((x, y) => y.at - x.at))
    if (!out.some((o) => sameItem(o, e))) out.push(e);
  return out.slice(0, RECENT_MAX);
}

/**
 * Remembers a pick and stores the result as far as storage allows. The pick goes on top of
 * the stored list, which another tab may have added to, merged with `list`, the palette's
 * own copy, which keeps the picks of this visit when storage refuses them. The returned
 * list is the new copy.
 */
export function recordRecent(list: readonly RecentEntry[], entry: RecentEntry): RecentEntry[] {
  const next = addRecent(mergeRecent(loadRecent(), list), entry);
  save(RECENT_KEY, next);
  return next;
}

/** The key of an item among the recent picks. */
export const recentKey = (provider: string, id: string) => `${provider}\u0000${id}`;
