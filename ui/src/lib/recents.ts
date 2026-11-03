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

/**
 * `list` with `entry` first and an earlier pick of the same item dropped. With `perDataset`
 * the item is the same only on the same dataset, so a pick on one dataset keeps the pick of
 * the same IRI or name on another.
 */
export function addRecent(
  list: readonly RecentEntry[],
  entry: RecentEntry,
  perDataset = true,
): RecentEntry[] {
  const same = (e: RecentEntry) =>
    e.provider === entry.provider && e.id === entry.id && (!perDataset || e.ds === entry.ds);
  return [entry, ...list.filter((e) => !same(e))].slice(0, RECENT_MAX);
}

/**
 * Remembers a pick on top of `list`, the palette's own copy, and stores the result as far as
 * storage allows. The returned list is the new copy, also when storage refuses it.
 */
export function recordRecent(
  list: readonly RecentEntry[],
  entry: RecentEntry,
  perDataset = true,
): RecentEntry[] {
  const next = addRecent(list, entry, perDataset);
  save(RECENT_KEY, next);
  return next;
}

/** The key of an item among the recent picks. */
export const recentKey = (provider: string, id: string) => `${provider}\u0000${id}`;
