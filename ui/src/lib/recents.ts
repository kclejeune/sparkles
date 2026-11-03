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

/** `list` with `entry` first and an earlier pick of the same item dropped. */
export function addRecent(list: readonly RecentEntry[], entry: RecentEntry): RecentEntry[] {
  return [
    entry,
    ...list.filter((e) => !(e.provider === entry.provider && e.id === entry.id)),
  ].slice(0, RECENT_MAX);
}

/** Remembers a pick and returns the new list. */
export function recordRecent(entry: RecentEntry): RecentEntry[] {
  const next = addRecent(loadRecent(), entry);
  save(RECENT_KEY, next);
  return next;
}

/** The key of an item among the recent picks. */
export const recentKey = (provider: string, id: string) => `${provider}\u0000${id}`;
