// The layout map of the Pattern pane (spec U08 §4.4): where each element of a query's
// pattern sits, by the element's key (`v:person`, `i:http://…`, `k:BIND:…:0`). The map of
// an editor tab lives in memory while the tab is open, so a node keeps its place while the
// text around it changes. Each saved map is also kept in localStorage under a hash of the
// query's text without whitespace, so the same query opened again, from the history or a
// stored query, draws the same picture. At most 200 maps are kept there, the oldest
// dropped first. A stored query's layout is not saved on the server (U08 open question 1,
// decided for the browser).

import { hash } from './pattern';
import { load, save } from './storage';

export type Point = { x: number; y: number };
export type Layout = Record<string, Point>;

export const LAYOUT_KEY = 'sparkles.patternLayouts';
export const MAX_LAYOUTS = 200;

type Stored = { entries: { h: string; t: number; layout: Layout }[] };

/** The key a query's layout is stored under: its text with whitespace removed, hashed. */
export const textKey = (text: string) => hash(text.replace(/\s+/g, ''));

/** The layouts of every editor tab, in memory, and their copies in localStorage. */
export class LayoutStore {
  private tabs = new Map<string, Layout>();
  private now: () => number;

  constructor(now: () => number = Date.now) {
    this.now = now;
  }

  /** The tab's layout; a tab seen for the first time starts from the one stored for
   * `text`, if any. */
  get(tab: string, text: string): Layout {
    let l = this.tabs.get(tab);
    if (!l) {
      l = { ...(this.stored(text) ?? {}) };
      this.tabs.set(tab, l);
    }
    return l;
  }

  /** Replace the tab's layout and store it under `text`. */
  set(tab: string, text: string, layout: Layout): void {
    this.tabs.set(tab, layout);
    this.persist(text, layout);
  }

  /** Move a key's position to another key (a variable renamed). */
  rename(tab: string, from: string, to: string): void {
    const l = this.tabs.get(tab);
    if (!l || !l[from]) return;
    l[to] = l[from];
    delete l[from];
  }

  forget(tab: string): void {
    this.tabs.delete(tab);
  }

  /** The layout stored for `text`, if any. */
  stored(text: string): Layout | null {
    const h = textKey(text);
    return read().entries.find((e) => e.h === h)?.layout ?? null;
  }

  private persist(text: string, layout: Layout): void {
    const h = textKey(text);
    const s = read();
    const entries = s.entries.filter((e) => e.h !== h);
    entries.push({ h, t: this.now(), layout });
    entries.sort((a, b) => a.t - b.t);
    while (entries.length > MAX_LAYOUTS) entries.shift();
    save(LAYOUT_KEY, { entries });
  }
}

function read(): Stored {
  const s = load<Stored>(LAYOUT_KEY, { entries: [] });
  return s && Array.isArray(s.entries) ? s : { entries: [] };
}

/** The positions of the layout's keys that are still drawn: what a redraw keeps. */
export function kept(layout: Layout, keys: Iterable<string>): Layout {
  const out: Layout = {};
  for (const k of keys) if (layout[k]) out[k] = layout[k];
  return out;
}
