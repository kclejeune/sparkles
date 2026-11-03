// The command palette's own sources (ROADMAP item 631, docs/visualization/README.md, "App
// shell"): the commands pages register, the pages of the shell, the datasets, the open
// dataset's saved queries and a page's branches. Each is a launcher provider
// (lib/launcher.svelte.ts) ranked with lib/fuzzy.ts; the entity search keeps the server's
// ranking (lib/entity-search.ts).

import { goto } from '$app/navigation';
import * as api from './api';
import { app } from './app.svelte';
import {
  launcher,
  type LauncherCommand,
  type LauncherItem,
  type LauncherProvider,
} from './launcher.svelte';
import { adminTabs, datasetSwitchHref, navItems, navLink } from './shell';

/** The ranking, loaded with the first typed query rather than with the shell. */
const ranking = () => import('./fuzzy').then((m) => m.rank);

/** Rows of a group on an empty query outside a mode. */
export const SUGGEST = 6;
/** Rows of a group in its own mode with nothing typed after the prefix. */
export const LIST_MAX = 50;

/** A command as a row. */
export function commandItem(c: LauncherCommand, highlight?: [number, number][]): LauncherItem {
  const run = c.run;
  return {
    id: c.id,
    title: c.label,
    highlight,
    subtitle: c.subtitle,
    icon: c.icon ?? 'zap',
    shortcut: c.shortcut,
    ...(typeof run === 'function' ? { run } : { query: run.query }),
  };
}

/**
 * The registered commands of one group. A page's commands are suggested on an empty query;
 * the shell's show once something is typed or in the `>` mode.
 */
export function commandProvider(group: 'page' | 'global'): LauncherProvider {
  const pageGroup = group === 'page';
  return {
    id: pageGroup ? 'page-commands' : 'commands',
    label: pageGroup ? 'This page' : 'Commands',
    order: pageGroup ? 5 : 15,
    modes: ['>'],
    perDataset: false,
    watch: () => launcher.activeCommands().filter((c) => (c.group ?? 'page') === group).length,
    async search(q, { mode, recent }) {
      const list = launcher.activeCommands().filter((c) => (c.group ?? 'page') === group);
      if (!q) {
        if (mode === '>') return list.slice(0, LIST_MAX).map((c) => commandItem(c));
        return pageGroup ? list.slice(0, SUGGEST).map((c) => commandItem(c)) : [];
      }
      const rank = await ranking();
      return rank(list, q, (c) => ({ id: c.id, title: c.label, keywords: c.keywords }), recent)
        .slice(0, 8)
        .map(({ item, match }) => commandItem(item, match.ranges));
    },
    revive(e) {
      const c = launcher.activeCommands().find((x) => x.id === e.id);
      return c && (c.group ?? 'page') === group ? commandItem(c) : null;
    },
  };
}

/** The shell's pages, with the open dataset's own page. */
export function pageProvider(base: string, backupsVisible: () => boolean): LauncherProvider {
  const pages = (ds: string | null) => {
    const out: (LauncherItem & { keywords?: string[] })[] = navItems().map((n) => ({
      id: n.match ?? n.href,
      title: n.label,
      icon: n.icon,
      href: navLink(base, n.href, ds),
    }));
    out.push({
      id: '/embedding',
      title: 'Embedding map',
      icon: 'similar',
      href: navLink(base, '/embedding', ds),
      keywords: ['vectors', 'projection', 'umap'],
    });
    // the Admin page's tabs, each its own row
    for (const t of adminTabs(backupsVisible()))
      out.push({
        id: t.href,
        title: `Admin › ${t.label}`,
        icon: t.icon,
        href: `${base}${t.href}`,
        keywords:
          t.href === '/admin/tokens' ? ['access tokens', 'api keys'] : [t.label.toLowerCase()],
      });
    if (ds) {
      out.push({
        id: '/dataset',
        title: `Dataset ${ds}`,
        icon: 'database',
        href: `${base}/datasets/${encodeURIComponent(ds)}`,
      });
    }
    return out;
  };
  return {
    id: 'pages',
    label: 'Go to',
    order: 30,
    modes: ['>'],
    perDataset: false,
    watch: backupsVisible,
    async search(q, { ds, mode, recent }) {
      const all = pages(ds);
      if (!q) return mode === '>' ? all : mode ? [] : all.slice(0, 4);
      const rank = await ranking();
      return rank(all, q, (p) => ({ id: p.id, title: p.title, keywords: p.keywords }), recent)
        .slice(0, 6)
        .map(({ item, match }) => ({ ...item, highlight: match.ranges }));
    },
    revive(e, { ds }) {
      return pages(ds).find((p) => p.id === e.id) ?? null;
    },
  };
}

/** Opens dataset `name`, on the same page when the page names its dataset in the address. */
export function switchDataset(name: string) {
  const href = datasetSwitchHref(new URL(location.href), name);
  app.setDataset(name);
  if (href) void goto(href);
}

/** The server's datasets: `#` lists them, and a typed query finds them. */
export function datasetProvider(): LauncherProvider {
  const item = (d: api.DatasetInfo, highlight?: [number, number][]): LauncherItem => ({
    id: d.name,
    title: d.name,
    highlight,
    subtitle: d.name === app.current ? 'Open now' : undefined,
    tag: 'dataset',
    icon: 'database',
    run: () => switchDataset(d.name),
  });
  return {
    id: 'datasets',
    label: 'Datasets',
    order: 25,
    modes: ['#'],
    perDataset: false,
    watch: () => app.datasets,
    async search(q, { mode, recent }) {
      if (!q) return mode === '#' ? app.datasets.slice(0, LIST_MAX).map((d) => item(d)) : [];
      const rank = await ranking();
      return rank(app.datasets, q, (d) => ({ id: d.name, title: d.name }), recent)
        .slice(0, 6)
        .map(({ item: d, match }) => item(d, match.ranges));
    },
    revive(e) {
      const d = app.datasets.find((x) => x.name === e.id);
      return d && d.name !== app.current ? item(d) : null;
    },
  };
}

/** The open dataset's stored queries, cached for a minute. */
export function savedQueryProvider(base: string): LauncherProvider {
  let cache: { ds: string; at: number; list: Promise<api.StoredQuery[]> } | null = null;
  const list = (ds: string) => {
    if (!cache || cache.ds !== ds || Date.now() - cache.at > 60_000) {
      const p = api.storedQueries(ds).catch((e) => {
        // servers without stored queries answer 404
        if (e instanceof api.ApiError && e.status === 404) return [];
        if (cache?.list === p) cache = null;
        throw e;
      });
      cache = { ds, at: Date.now(), list: p };
    }
    return cache.list;
  };
  const href = (ds: string, name: string) =>
    `${base}/query?ds=${encodeURIComponent(ds)}&saved=${encodeURIComponent(name)}`;
  return {
    id: 'saved-queries',
    label: ({ ds }) => (ds ? `Saved queries in ${ds}` : 'Saved queries'),
    order: 20,
    modes: ['?'],
    active: ({ ds }) => !!ds,
    async search(q, { ds, mode, recent }) {
      if (!ds || (!q && mode !== '?')) return [];
      const all = await list(ds);
      const rank = await ranking();
      const ranked = q
        ? rank(
            all,
            q,
            (s) => ({ id: s.name, title: s.name, keywords: s.description ? [s.description] : [] }),
            recent,
          ).slice(0, 6)
        : all.slice(0, LIST_MAX).map((s) => ({ item: s, match: { score: 0, ranges: [] } }));
      return ranked.map(({ item: s, match }) => ({
        id: s.name,
        title: s.name,
        highlight: match.ranges,
        subtitle: s.description || undefined,
        tag: 'saved',
        icon: 'code',
        href: href(ds, s.name),
      }));
    },
  };
}

/**
 * A page's branches, which `#` lists beside the datasets. The page says how to read them and
 * how to open one, as its own branch select does.
 */
export function branchProvider(opts: {
  list: () => Promise<string[]> | string[];
  current: () => string;
  choose: (name: string) => void;
}): LauncherProvider {
  return {
    id: 'branches',
    label: ({ ds }) => (ds ? `Branches of ${ds}` : 'Branches'),
    order: 26,
    modes: ['#'],
    recent: false,
    active: ({ ds }) => !!ds,
    watch: () => opts.list(),
    async search(q, { mode }) {
      if (!q && mode !== '#') return [];
      const names = await opts.list();
      const cur = opts.current();
      const rank = await ranking();
      const ranked = q
        ? rank(names, q, (n) => ({ id: n, title: n })).slice(0, 6)
        : names.slice(0, LIST_MAX).map((n) => ({ item: n, match: { score: 0, ranges: [] } }));
      return ranked.map(({ item: n, match }) => ({
        id: n,
        title: n,
        highlight: match.ranges as [number, number][],
        subtitle: n === cur ? 'Open now' : undefined,
        tag: 'branch',
        icon: 'branch',
        run: () => opts.choose(n),
      }));
    },
  };
}

/** The shell's own commands, on every page. */
export function shellCommands(
  base: string,
  can: { createDataset: () => boolean; backups: () => boolean },
): LauncherCommand[] {
  const ds = () => app.current;
  const go = (path: string) => () => void goto(base + path);
  const theme = (t: 'light' | 'dark' | 'system', label: string): LauncherCommand => ({
    id: `theme.${t}`,
    label: `Theme: ${label}`,
    group: 'global',
    icon: t === 'light' ? 'sun' : t === 'dark' ? 'moon' : 'monitor',
    keywords: ['appearance', 'colours', 'colors', t === 'system' ? 'auto' : `${t} mode`],
    active: () => app.theme !== t,
    run: () => app.setTheme(t),
  });
  return [
    {
      id: 'dataset.switch',
      label: 'Switch dataset or branch…',
      group: 'global',
      icon: 'swap',
      shortcut: '#',
      keywords: ['open dataset', 'change'],
      active: () => app.datasets.length > 0,
      run: { query: '#' },
    },
    {
      id: 'saved.open',
      label: 'Open saved query…',
      group: 'global',
      icon: 'code',
      shortcut: '?',
      keywords: ['stored queries'],
      active: () => !!ds(),
      run: { query: '?' },
    },
    theme('light', 'light'),
    theme('dark', 'dark'),
    theme('system', 'follow the system'),
    {
      id: 'go.settings',
      label: 'Open dataset settings',
      group: 'global',
      icon: 'filter',
      keywords: ['prefixes', 'forms', 'validation', 'reasoning', 'preferences'],
      active: () => !!ds(),
      run: () => void goto(`${base}/datasets/${encodeURIComponent(ds()!)}?tab=settings`),
    },
    {
      id: 'go.server',
      label: 'Open server status',
      group: 'global',
      icon: 'server',
      keywords: ['metrics', 'health', 'tasks'],
      run: go('/admin/server'),
    },
    {
      id: 'go.tokens',
      label: 'Open access tokens',
      group: 'global',
      icon: 'key',
      keywords: ['api keys', 'credentials'],
      run: go('/admin/tokens'),
    },
    {
      id: 'go.backups',
      label: 'Open backups',
      group: 'global',
      icon: 'archive',
      keywords: ['restore', 'snapshots'],
      active: can.backups,
      run: go('/admin/backups'),
    },
    {
      id: 'dataset.new',
      label: 'New dataset…',
      group: 'global',
      icon: 'plus',
      keywords: ['create dataset', 'add dataset'],
      active: can.createDataset,
      run: go('/datasets?new=1'),
    },
    {
      id: 'search.page',
      label: 'Search this page',
      group: 'global',
      icon: 'search',
      shortcut: '/',
      keywords: ['filter', 'find'],
      active: () => launcher.hasPageSearch,
      run: () => void launcher.focusPageSearch(),
    },
  ];
}
