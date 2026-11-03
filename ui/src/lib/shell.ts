// The app shell's navigation: the pages of the rail and the drawer, in two groups, and
// how a link carries the open dataset.

export interface NavItem {
  href: string;
  label: string;
  icon: string;
  /** The working pages come first; the administration pages follow a rule. */
  group: 'work' | 'admin';
}

/** The pages whose link carries the open dataset as `?ds=`. */
const CARRIES_DATASET = new Set([
  '/ask',
  '/explore',
  '/browse',
  '/ontology',
  '/thesaurus',
  '/similar',
  '/memory',
]);

export function navItems(backupsVisible: boolean): NavItem[] {
  return [
    { href: '/query', label: 'Query', icon: 'query', group: 'work' },
    { href: '/ask', label: 'Ask', icon: 'sparkle', group: 'work' },
    { href: '/explore', label: 'Explore', icon: 'explore', group: 'work' },
    { href: '/browse', label: 'Browse', icon: 'book', group: 'work' },
    { href: '/ontology', label: 'Ontology', icon: 'tree', group: 'work' },
    { href: '/thesaurus', label: 'Thesaurus', icon: 'layers', group: 'work' },
    { href: '/similar', label: 'Similar', icon: 'similar', group: 'work' },
    { href: '/memory', label: 'Memory', icon: 'brain', group: 'work' },
    { href: '/datasets', label: 'Datasets', icon: 'database', group: 'admin' },
    ...(backupsVisible
      ? [{ href: '/backups', label: 'Backups', icon: 'archive', group: 'admin' as const }]
      : []),
    { href: '/server', label: 'Server', icon: 'server', group: 'admin' },
  ];
}

/** The link of `href` under `base`, with the open dataset where the page takes one. */
export function navLink(base: string, href: string, ds: string | null): string {
  const url = base + href;
  return CARRIES_DATASET.has(href) && ds ? `${url}?ds=${encodeURIComponent(ds)}` : url;
}

/** Whether `pathname` is the page of `href` or one under it. */
export function navActive(base: string, href: string, pathname: string): boolean {
  const full = base + href;
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
