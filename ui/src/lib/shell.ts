// The app shell's navigation: the pages of the rail and the drawer, in two groups, and
// how a link carries the open dataset.

export interface NavLink {
  href: string;
  label: string;
  icon: string;
  /** The path whose pages make the link current, when it is not `href`. */
  match?: string;
  /** Current only on `match` itself, not on the pages under it. */
  exact?: boolean;
}

export interface NavItem extends NavLink {
  /** The working pages come first; the administration pages follow a rule. */
  group: 'work' | 'admin';
  /**
   * The pages of a group, shown under it in the rail and the drawer and in a flyout on the
   * icon rail (ROADMAP item 633). A page has at most one tab row, so a merged area's pages
   * are links here rather than tabs. Making a group separate entries is a change of this list.
   */
  children?: NavLink[];
}

/** The pages whose link carries the open dataset as `?ds=`. */
const CARRIES_DATASET = new Set([
  '/explore',
  '/browse',
  '/ontology',
  '/thesaurus',
  '/similar',
  '/similar/map',
  '/memory',
]);

/** The Admin group's pages; Backups only for a caller who may see backups. */
export function adminPages(backupsVisible: boolean): NavLink[] {
  return [
    { href: '/admin/server', label: 'Server', icon: 'server' },
    ...(backupsVisible ? [{ href: '/admin/backups', label: 'Backups', icon: 'archive' }] : []),
    { href: '/admin/tokens', label: 'Tokens', icon: 'key' },
  ];
}

export function navItems(backupsVisible = false): NavItem[] {
  return [
    { href: '/query', label: 'Query', icon: 'query', group: 'work' },
    { href: '/explore', label: 'Explore', icon: 'explore', group: 'work' },
    { href: '/browse', label: 'Browse', icon: 'book', group: 'work' },
    { href: '/ontology', label: 'Ontology', icon: 'tree', group: 'work' },
    { href: '/thesaurus', label: 'Thesaurus', icon: 'layers', group: 'work' },
    {
      href: '/similar',
      label: 'Similar',
      icon: 'similar',
      group: 'work',
      children: [
        { href: '/similar', label: 'Search', icon: 'search', exact: true },
        { href: '/similar/map', label: 'Map', icon: 'map' },
      ],
    },
    { href: '/memory', label: 'Memory', icon: 'brain', group: 'work' },
    { href: '/datasets', label: 'Datasets', icon: 'database', group: 'admin' },
    {
      href: '/admin/server',
      label: 'Admin',
      icon: 'server',
      group: 'admin',
      match: '/admin',
      children: adminPages(backupsVisible),
    },
  ];
}

/** The link of `href` under `base`, with the open dataset where the page takes one. */
export function navLink(base: string, href: string, ds: string | null): string {
  const url = base + href;
  return CARRIES_DATASET.has(href) && ds ? `${url}?ds=${encodeURIComponent(ds)}` : url;
}

/** Whether `pathname` is the page of `href` or one under it. */
export function navActive(base: string, href: string, pathname: string, exact = false): boolean {
  const full = base + href;
  if (pathname === full) return true;
  if (exact) return false;
  return pathname === full || pathname.startsWith(full + '/');
}

/**
 * Where choosing dataset `ds` goes from `url`: a page that names its dataset in `?ds=`
 * reopens on the new one, without the parameters that belonged to the old one (an
 * entity, a graph, a branch); any other page stays where it is.
 */
export function datasetSwitchHref(url: URL, ds: string): string | null {
  if (!url.searchParams.has('ds')) return null;
  return `${url.pathname}?ds=${encodeURIComponent(ds)}`;
}
