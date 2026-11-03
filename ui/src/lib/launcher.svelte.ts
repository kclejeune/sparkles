// The top bar's search and launcher (docs/visualization/README.md, "Web UI"). The bar's
// field is the launcher's input: Ctrl+K, Cmd+K and `/` focus it from anywhere, and the
// providers registered here answer what is typed into it, each as a group of items.
//
// This is the extension point of the command palette (ROADMAP item 631): a page or a
// feature registers a provider for as long as it is mounted, and the bar shows its items
// beside the others, in `order`. The shell registers one provider of its own, the entity
// search of `GET /$/ontology/{ds}/search` (lib/entity-search.ts), and the palette's own
// sources and commands (lib/palette.svelte.ts).

import { onMount } from 'svelte';

/** One row of the launcher's list. */
export interface LauncherItem {
  /** Unique within its provider. */
  id: string;
  title: string;
  /** Ranges of `title` to highlight, in UTF-16 code units, end exclusive. */
  highlight?: [number, number][];
  subtitle?: string;
  /** A short tag at the end of the row, such as the kind of an entity. */
  tag?: string;
  /** Where Enter or a click goes. */
  href?: string;
  /**
   * What Enter or a click does when there is no `href`. The focus goes back to where it was
   * before the launcher opened, then `run` is called, so a dialog it opens takes the focus.
   */
  run?: () => void | Promise<void>;
  /** What Enter or a click writes into the field instead, such as `#` to list datasets. */
  query?: string;
  /** A key the item answers to elsewhere, such as `Mod+Enter`; shown, not bound here. */
  shortcut?: string;
  /** An icon name of `Icon.svelte`, shown before the title. */
  icon?: string;
}

/** The prefixes that show one kind of item only. */
export type LauncherMode = '>' | '@' | '#' | '?';

export const MODES: { mode: LauncherMode | null; label: string; title: string }[] = [
  { mode: null, label: 'All', title: 'Everything' },
  { mode: '>', label: 'Commands', title: 'Commands and pages' },
  { mode: '@', label: 'Entities', title: 'Classes, properties, individuals and concepts' },
  { mode: '#', label: 'Datasets', title: 'Datasets and branches' },
  { mode: '?', label: 'Saved', title: 'Saved queries' },
];

/** The mode prefix of what was typed into the field, and the query after it. */
export function parseMode(raw: string): { mode: LauncherMode | null; query: string } {
  const t = raw.trimStart();
  const c = t[0];
  if (c === '>' || c === '@' || c === '#' || c === '?')
    return { mode: c, query: t.slice(1).trim() };
  return { mode: null, query: t.trim() };
}

/** A recent pick as it is remembered: enough to show it and to find it again. */
export interface RecentEntry {
  provider: string;
  id: string;
  title: string;
  subtitle?: string;
  tag?: string;
  icon?: string;
  href?: string;
  /** The dataset that was open, for items that belong to one. */
  ds: string | null;
  at: number;
}

export interface LauncherContext {
  /** The dataset the UI has open, or null. */
  ds: string | null;
  signal: AbortSignal;
  /**
   * The mode the field is in. In a provider's own mode an empty query lists all of its items;
   * outside it an empty query may answer suggestions or nothing.
   */
  mode?: LauncherMode | null;
  /** The ids of this provider's recent picks, newest first, for a tie-break in ranking. */
  recent?: ReadonlySet<string>;
}

export interface LauncherProvider {
  id: string;
  /** The heading of the provider's group. */
  label: string | ((ctx: { ds: string | null }) => string);
  /** Groups show in ascending order. */
  order: number;
  /** Whether the provider answers in this context, such as only with a dataset open. */
  active?: (ctx: { ds: string | null }) => boolean;
  /** Items for `query`; an empty query may answer recent or suggested items. */
  search(query: string, ctx: LauncherContext): Promise<LauncherItem[]>;
  /** The mode prefixes that show this provider; outside a mode, every provider shows. */
  modes?: LauncherMode[];
  /**
   * Reads the state the provider's items come from, so that an open launcher asks again when
   * it changes, such as a branch list that arrives after the launcher opened.
   */
  watch?: () => unknown;
  /** Whether a pick is remembered among the recent items (default true). */
  recent?: boolean;
  /** Whether a recent pick belongs to the dataset open then (default true). */
  perDataset?: boolean;
  /**
   * The item of a recent pick, or null when it no longer applies. Without this a pick with
   * an `href` opens it again and any other is not shown.
   */
  revive?(entry: RecentEntry, ctx: { ds: string | null }): LauncherItem | null;
}

/** A command a page or the shell offers in the palette. */
export interface LauncherCommand {
  /** Stable across visits, such as `query.run`: recent picks find the command by it. */
  id: string;
  label: string;
  /** "This page" for a page's commands, "Commands" for the shell's; default `page`. */
  group?: 'page' | 'global';
  icon?: string;
  shortcut?: string;
  /** More words the command answers to, matched at the start of a word. */
  keywords?: string[];
  subtitle?: string;
  /** Whether the command applies now; read each time the palette searches. */
  active?: () => boolean;
  /** Runs the command. A string sets the field instead, such as `#` to list datasets. */
  run: (() => void | Promise<void>) | { query: string };
}

export interface LauncherGroup {
  provider: string;
  label: string;
  items: LauncherItem[];
  error?: string;
  /** For the recent picks: the provider each item came from. */
  from?: string[];
}

class LauncherState {
  /** Whether the results are showing (and, on a phone, the field is expanded). */
  open = $state(false);
  query = $state('');
  /** The field's placeholder while a page names what its search finds, such as Explore's. */
  hint = $state<string | null>(null);
  // raw: a deep proxy would wrap each provider and command, and an unregister compares
  // the caller's own object with what the list holds
  #providers = $state.raw<LauncherProvider[]>([]);
  #commands = $state.raw<LauncherCommand[]>([]);
  #pageSearch = $state<(() => boolean | void) | null>(null);
  #focus: (() => void) | null = null;

  get providers(): readonly LauncherProvider[] {
    return this.#providers;
  }

  /** Adds a provider; the returned function removes it again. */
  register(p: LauncherProvider): () => void {
    this.#providers = [...this.#providers.filter((x) => x.id !== p.id), p].sort(
      (a, b) => a.order - b.order,
    );
    return () => {
      this.#providers = this.#providers.filter((x) => x !== p);
    };
  }

  get commands(): readonly LauncherCommand[] {
    return this.#commands;
  }

  /**
   * Adds commands; the returned function removes them again. A command whose id is already
   * registered replaces the earlier one, as a page that mounts again does.
   */
  command(cmds: LauncherCommand | LauncherCommand[]): () => void {
    const list = Array.isArray(cmds) ? cmds : [cmds];
    const ids = new Set(list.map((c) => c.id));
    this.#commands = [...this.#commands.filter((c) => !ids.has(c.id)), ...list];
    return () => {
      this.#commands = this.#commands.filter((c) => !list.includes(c));
    };
  }

  /** The commands that apply now, in the order they were registered. */
  activeCommands(): LauncherCommand[] {
    return this.#commands.filter((c) => {
      try {
        return !c.active || c.active();
      } catch {
        return false;
      }
    });
  }

  /**
   * A page's own search field, which `/` focuses instead of opening the launcher. `focus`
   * returns false when the field is not showing, and `/` then opens the launcher. The
   * returned function removes it again.
   */
  pageSearch(focus: () => boolean | void): () => void {
    this.#pageSearch = focus;
    return () => {
      if (this.#pageSearch === focus) this.#pageSearch = null;
    };
  }

  /** Focuses the page's own search; false when the page has none. */
  focusPageSearch(): boolean {
    return !!this.#pageSearch && this.#pageSearch() !== false;
  }

  get hasPageSearch(): boolean {
    return this.#pageSearch != null;
  }

  /** The top bar binds the function that focuses its field. */
  bindFocus(f: (() => void) | null) {
    this.#focus = f;
  }

  /** Opens the launcher and focuses its field. */
  show() {
    this.open = true;
    this.#focus?.();
  }

  close() {
    this.open = false;
  }
}

export const launcher = new LauncherState();

/**
 * Registers a component's commands while it is mounted. Call it during the component's
 * initialization; `active` predicates keep the commands current as the page's state changes.
 */
export function useCommands(cmds: () => LauncherCommand[]) {
  onMount(() => launcher.command(cmds()));
}

/** Registers a component's provider while it is mounted. */
export function useProvider(p: LauncherProvider) {
  onMount(() => launcher.register(p));
}

/** Sets the top bar field's placeholder while a component is mounted. */
export function useHint(hint: () => string | null) {
  $effect(() => {
    const h = hint();
    launcher.hint = h;
    return () => {
      if (launcher.hint === h) launcher.hint = null;
    };
  });
}

/** Makes `/` focus a component's own search field while it is mounted. */
export function usePageSearch(focus: () => boolean | void) {
  onMount(() => launcher.pageSearch(focus));
}

const KEY_NAMES: Record<string, [mac: string, other: string]> = {
  mod: ['⌘', 'Ctrl'],
  ctrl: ['⌃', 'Ctrl'],
  alt: ['⌥', 'Alt'],
  shift: ['⇧', 'Shift'],
  enter: ['↵', 'Enter'],
};

/** The keys of a shortcut such as `Shift+Alt+F` as the platform names them. */
export function shortcutKeys(shortcut: string, mac: boolean): string[] {
  return shortcut.split('+').map((k) => {
    const n = KEY_NAMES[k.toLowerCase()];
    return n ? n[mac ? 0 : 1] : k.length === 1 ? k.toUpperCase() : k;
  });
}

/** Whether `e` happened in a place where `/` types a character. */
export function isTypingTarget(target: EventTarget | null): boolean {
  const t = target as Partial<HTMLInputElement> | null;
  if (!t || typeof t.tagName !== 'string') return false;
  if (t.isContentEditable || t.closest?.('.cm-editor')) return true;
  const tag = t.tagName.toUpperCase();
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (tag !== 'INPUT') return false;
  const type = t.type ?? 'text';
  return !['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file'].includes(
    type,
  );
}

type KeyLike = Pick<
  KeyboardEvent,
  'key' | 'ctrlKey' | 'metaKey' | 'altKey' | 'shiftKey' | 'defaultPrevented' | 'target'
>;

/** Whether `e` is Ctrl+K or Cmd+K, which always opens the launcher. */
export function isPaletteKey(e: KeyLike): boolean {
  return (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k';
}

/**
 * Whether `e` is the launcher's key: Ctrl+K or Cmd+K, or `/` outside a text field. On a page
 * with its own search (`launcher.pageSearch`), `/` focuses that one instead.
 */
export function isLauncherKey(e: KeyLike): boolean {
  if (e.defaultPrevented && e.key === '/') return false;
  if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k')
    return true;
  return (
    e.key === '/' && !e.ctrlKey && !e.metaKey && !e.altKey && !isTypingTarget(e.target ?? null)
  );
}

/** The ranges of `text` as alternating plain and highlighted parts. */
export function highlightParts(
  text: string,
  ranges: [number, number][] = [],
): { text: string; hit: boolean }[] {
  const out: { text: string; hit: boolean }[] = [];
  let at = 0;
  for (const [a, b] of [...ranges].sort((x, y) => x[0] - y[0])) {
    const s = Math.max(a, at);
    const e = Math.min(b, text.length);
    if (e <= s) continue;
    if (s > at) out.push({ text: text.slice(at, s), hit: false });
    out.push({ text: text.slice(s, e), hit: true });
    at = e;
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}
