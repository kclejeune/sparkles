import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { app } from './app.svelte';
import {
  launcher,
  parseMode,
  shortcutKeys,
  type LauncherCommand,
  type LauncherItem,
  type RecentEntry,
} from './launcher.svelte';
import { commandProvider, pageProvider, shellCommands } from './palette.svelte';
import { addRecent, loadRecent, RECENT_KEY, RECENT_MAX, recordRecent } from './recents';

const ctx = (mode: '>' | null = null) => ({
  ds: 'docs',
  signal: new AbortController().signal,
  mode,
});

/** The query page's commands as it registers them (routes/query/+page.svelte). */
const queryPage: LauncherCommand[] = [
  { id: 'query.run', label: 'Run query', keywords: ['execute'], run: () => {} },
  {
    id: 'query.format',
    label: 'Format query',
    keywords: ['pretty print', 'indent', 'tidy'],
    run: () => {},
  },
  { id: 'query.explain', label: 'Explain query', keywords: ['plan', 'algebra'], run: () => {} },
  { id: 'query.tab', label: 'New query tab', run: () => {} },
  { id: 'query.save', label: 'Save query…', keywords: ['stored query'], run: () => {} },
  {
    id: 'query.history',
    label: 'Show or hide history',
    keywords: ['time slider', 'commits'],
    run: () => {},
  },
  {
    id: 'query.pattern',
    label: 'Show or hide the pattern pane',
    keywords: ['visual query'],
    run: () => {},
  },
];

describe('parseMode', () => {
  it('splits a mode prefix from the query', () => {
    expect(parseMode('>form')).toEqual({ mode: '>', query: 'form' });
    expect(parseMode('  # go ')).toEqual({ mode: '#', query: 'go' });
    expect(parseMode('?')).toEqual({ mode: '?', query: '' });
    expect(parseMode('@pers')).toEqual({ mode: '@', query: 'pers' });
    expect(parseMode('person')).toEqual({ mode: null, query: 'person' });
    expect(parseMode('a>b')).toEqual({ mode: null, query: 'a>b' });
  });
});

describe('shortcutKeys', () => {
  it('names the keys for the platform', () => {
    expect(shortcutKeys('Mod+Enter', true)).toEqual(['⌘', '↵']);
    expect(shortcutKeys('Mod+Enter', false)).toEqual(['Ctrl', 'Enter']);
    expect(shortcutKeys('Shift+Alt+F', false)).toEqual(['Shift', 'Alt', 'F']);
    expect(shortcutKeys('h', false)).toEqual(['H']);
  });
});

describe('the command registry', () => {
  it('registers, replaces by id, filters by active and unregisters', () => {
    let on = true;
    const offA = launcher.command([
      { id: 't.a', label: 'A', run: () => {} },
      { id: 't.b', label: 'B', active: () => on, run: () => {} },
    ]);
    const offA2 = launcher.command({ id: 't.a', label: 'A again', run: () => {} });
    const mine = () => launcher.activeCommands().filter((c) => c.id.startsWith('t.'));
    expect(mine().map((c) => c.label)).toEqual(['B', 'A again']);
    on = false;
    expect(mine().map((c) => c.label)).toEqual(['A again']);
    offA();
    expect(mine().map((c) => c.label)).toEqual(['A again']);
    offA2();
    expect(mine()).toEqual([]);
  });

  it('skips a command whose active predicate throws', () => {
    const off = launcher.command({
      id: 't.x',
      label: 'X',
      active: () => {
        throw new Error('not ready');
      },
      run: () => {},
    });
    expect(launcher.activeCommands().some((c) => c.id === 't.x')).toBe(false);
    off();
  });

  it('lets a page search take `/` and gives it back', () => {
    let focused = 0;
    let showing = true;
    const off = launcher.pageSearch(() => {
      if (!showing) return false;
      focused++;
    });
    expect(launcher.focusPageSearch()).toBe(true);
    expect(focused).toBe(1);
    showing = false;
    expect(launcher.focusPageSearch()).toBe(false);
    off();
    expect(launcher.hasPageSearch).toBe(false);
    expect(launcher.focusPageSearch()).toBe(false);
  });
});

describe('ranking real commands', () => {
  let offs: (() => void)[] = [];
  beforeEach(() => {
    app.current = 'docs';
    app.datasets = [{ name: 'docs', quads: 10 } as (typeof app.datasets)[number]];
    offs = [
      launcher.command(shellCommands('/ui', { createDataset: () => true, backups: () => true })),
      launcher.command(queryPage),
    ];
  });
  afterEach(() => offs.forEach((f) => f()));

  const titles = async (q: string, mode: '>' | null = '>') => {
    const page = await commandProvider('page').search(q, ctx(mode));
    const global = await commandProvider('global').search(q, ctx(mode));
    const pages = await pageProvider('/ui', () => true).search(q, ctx(mode));
    return [...page, ...global, ...pages].map((i: LauncherItem) => i.title);
  };

  it('`>form` finds Format query and the dataset settings only', async () => {
    expect(await titles('form')).toEqual(['Format query', 'Open dataset settings']);
  });

  it('a theme answers to its name, not to stray letters', async () => {
    expect(await titles('dark')).toEqual(['Theme: dark']);
    // the theme in use is not offered again
    app.theme = 'system';
    expect(await titles('theme')).toEqual(['Theme: dark', 'Theme: light']);
  });

  it('one typo in a longer word still finds the command', async () => {
    expect(await titles('explian')).toEqual(['Explain query']);
    expect((await titles('qeury')).slice(0, 2)).toEqual(['Run query', 'New query tab']);
    // a shorter word must be spelled as it is
    expect(await titles('qury')).toEqual([]);
  });

  it('an empty query suggests the page commands and pages, the `>` mode lists all', async () => {
    const all = await titles('', null);
    expect(all.slice(0, 6)).toEqual(queryPage.slice(0, 6).map((c) => c.label));
    expect(all).not.toContain('Theme: dark');
    expect(await titles('', '>')).toContain('Theme: dark');
  });

  it('a recent command is revived only while it is registered and active', () => {
    const p = commandProvider('global');
    const e: RecentEntry = { provider: 'commands', id: 'theme.dark', title: 'x', ds: null, at: 1 };
    app.theme = 'light';
    expect(p.revive!(e, { ds: 'docs' })?.title).toBe('Theme: dark');
    app.theme = 'dark';
    expect(p.revive!(e, { ds: 'docs' })).toBeNull();
    expect(p.revive!({ ...e, id: 'nope' }, { ds: 'docs' })).toBeNull();
  });
});

describe('recent picks', () => {
  const entry = (id: string, at = 1): RecentEntry => ({
    provider: 'pages',
    id,
    title: id,
    href: `/ui${id}`,
    ds: null,
    at,
  });

  it('puts the newest first, drops a repeat and keeps at most the cap', () => {
    let list: RecentEntry[] = [];
    for (let i = 0; i < RECENT_MAX + 5; i++) list = addRecent(list, entry(`/p${i}`, i));
    expect(list).toHaveLength(RECENT_MAX);
    expect(list[0].id).toBe(`/p${RECENT_MAX + 4}`);
    list = addRecent(list, entry('/p10', 99));
    expect(list[0]).toMatchObject({ id: '/p10', at: 99 });
    expect(list.filter((e) => e.id === '/p10')).toHaveLength(1);
  });

  it('survives storage that is missing, corrupt or throwing', () => {
    const store = new Map<string, string>();
    const fake = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    };
    vi.stubGlobal('localStorage', fake);
    expect(loadRecent()).toEqual([]);
    recordRecent([], entry('/a'));
    expect(loadRecent().map((e) => e.id)).toEqual(['/a']);
    store.set(RECENT_KEY, '{not json');
    expect(loadRecent()).toEqual([]);
    store.set(RECENT_KEY, JSON.stringify([{ id: 1 }, entry('/b')]));
    expect(loadRecent().map((e) => e.id)).toEqual(['/b']);
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    });
    expect(loadRecent()).toEqual([]);
    // the palette's own copy keeps every pick of the visit while storage refuses them
    let list = recordRecent([], entry('/c'));
    list = recordRecent(list, entry('/d'));
    expect(list.map((e) => e.id)).toEqual(['/d', '/c']);
    vi.unstubAllGlobals();
  });

  it('keeps the same item picked on two datasets apart', () => {
    const on = (ds: string, at: number): RecentEntry => ({
      provider: 'entities',
      id: 'http://example.org/Person',
      title: 'Person',
      ds,
      at,
    });
    let list = addRecent([], on('a', 1));
    list = addRecent(list, on('b', 2));
    expect(list.map((e) => e.ds)).toEqual(['b', 'a']);
    list = addRecent(list, on('a', 3));
    expect(list.map((e) => `${e.ds}${e.at}`)).toEqual(['a3', 'b2']);
    // a pick that belongs to no dataset is one item everywhere
    list = addRecent(addRecent([], entry('/x', 1)), { ...entry('/x', 2), ds: 'other' }, false);
    expect(list).toHaveLength(1);
  });
});
