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
  '/ask',
  '/explore',
  '/browse',
  '/workbench/ontology',
  '/workbench/thesaurus',
  '/workbench/shapes',
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

/** The Workbench group's editors (ROADMAP item 633), the place for authoring tools. */
export const WORKBENCH_PAGES: NavLink[] = [
  { href: '/workbench/ontology', label: 'Ontology', icon: 'tree' },
  { href: '/workbench/thesaurus', label: 'Thesaurus', icon: 'layers' },
  { href: '/workbench/shapes', label: 'SHACL shapes', icon: 'check' },
];

/** What the caller may see in the rail. */
export interface NavAccess {
  /** Backups: server admins, and dataset admins in a reduced form. */
  backupsVisible?: boolean;
  /** The Workbench: callers who may write the open dataset. */
  workbench?: boolean;
}

/**
 * The rail's pages in order. Readers get Browse; writers also get the Workbench's editors.
 * The Ask page of the ask-ui lane goes after Query.
 */
export function navItems({ backupsVisible = false, workbench = false }: NavAccess = {}): NavItem[] {
  return [
    { href: '/query', label: 'Query', icon: 'query', group: 'work' },
    { href: '/ask', label: 'Ask', icon: 'sparkle', group: 'work' },
    { href: '/explore', label: 'Explore', icon: 'explore', group: 'work' },
    { href: '/browse', label: 'Browse', icon: 'book', group: 'work' },
    ...(workbench
      ? [
          {
            href: '/workbench/ontology',
            label: 'Workbench',
            icon: 'pencil',
            group: 'work' as const,
            match: '/workbench',
            children: WORKBENCH_PAGES,
          },
        ]
      : []),
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
 * entity, a graph, a branch). A dataset's own page opens the new dataset's page on the same
 * tab. Any other page stays where it is.
 */
export function datasetSwitchHref(url: URL, ds: string): string | null {
  // a dataset's own page names it in the path, and keeps its tab
  const own = /^(.*\/datasets\/)[^/]+$/.exec(url.pathname);
  if (own) {
    const tab = url.searchParams.get('tab');
    return `${own[1]}${encodeURIComponent(ds)}${tab ? `?tab=${encodeURIComponent(tab)}` : ''}`;
  }
  if (!url.searchParams.has('ds')) return null;
  return `${url.pathname}?ds=${encodeURIComponent(ds)}`;
}
