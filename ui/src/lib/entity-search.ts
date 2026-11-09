// The entity search of `GET /$/ontology/{ds}/search` (docs/API.md, "Entity search"), and
// the launcher provider that shows its hits in the top bar.

import { request } from './api';
import { iriForm } from './iri-form.svelte';
import { displayEntity, WELL_KNOWN, type PrefixMap } from './rdf';
import type { LauncherItem, LauncherProvider } from './launcher.svelte';

export type EntitySearchKind =
  | 'class'
  | 'objectProperty'
  | 'dataProperty'
  | 'annotationProperty'
  | 'property'
  | 'individual'
  | 'datatype'
  | 'concept'
  | 'conceptScheme'
  | 'collection';

export interface EntityHit {
  type: 'entity';
  iri: string;
  kinds: string[];
  label: string | null;
  labelLang: string | null;
  shortForm: string | null;
  deprecated: boolean;
  match: {
    field: string;
    property: string | null;
    text: string;
    lang: string | null;
    kind: string;
    distance: number;
    tier: number;
    highlight: [number, number][];
  };
}

export interface EntitySearch {
  dataset: string;
  query: string;
  results: EntityHit[];
  more: boolean;
  next: number | null;
}

export async function searchEntities(
  ds: string,
  q: string,
  opts: {
    lang?: readonly string[];
    limit?: number;
    offset?: number;
    kind?: EntitySearchKind[];
    /** the graph to search, null for the default graph; the server's choice when absent */
    graph?: string | null;
    /** false to leave out the imported graphs; the server includes them when absent */
    imports?: boolean;
    signal?: AbortSignal;
  } = {},
): Promise<EntitySearch> {
  const p = new URLSearchParams({ q, limit: String(opts.limit ?? 10) });
  const languages = opts.lang ?? iriForm.languages;
  if (languages.length) p.set('lang', languages.join(','));
  if (opts.offset) p.set('offset', String(opts.offset));
  if (opts.kind?.length) p.set('kind', opts.kind.join(','));
  if (opts.graph !== undefined) p.set('graph', opts.graph ?? 'default');
  if (opts.imports === false) p.set('imports', 'false');
  const res = await request(`/$/ontology/${encodeURIComponent(ds)}/search?${p}`, {
    headers: { Accept: 'application/json' },
    signal: opts.signal,
  });
  return (await res.json()) as EntitySearch;
}

const KIND_NAME: Record<string, string> = {
  class: 'class',
  objectProperty: 'object property',
  dataProperty: 'data property',
  annotationProperty: 'annotation property',
  individual: 'individual',
  datatype: 'datatype',
  concept: 'concept',
  conceptScheme: 'concept scheme',
  collection: 'collection',
};

const FIELD_NAME: Record<string, string> = {
  synonym: 'synonym',
  shortForm: 'short form',
  iri: 'IRI',
  definition: 'definition',
};

/**
 * Where a hit opens: the entity's page in the ontology browser, for every kind. The
 * browser's Edit button opens the ontology or thesaurus editor on it.
 */
export function entityHref(base: string, ds: string, hit: Pick<EntityHit, 'iri' | 'kinds'>) {
  return `${base}/browse?ds=${encodeURIComponent(ds)}&iri=${encodeURIComponent(hit.iri)}`;
}

/** A hit as a launcher row: the label with its match, and what matched when not the label. */
export function entityItem(
  base: string,
  ds: string,
  h: EntityHit,
  prefixes: PrefixMap = WELL_KNOWN,
): LauncherItem {
  const onLabel = h.match.field === 'label';
  const title = displayEntity(h.iri, prefixes, h.label ? [{ '@value': h.label }] : undefined);
  const parts = [h.shortForm ?? h.iri];
  if (!onLabel) parts.push(`${FIELD_NAME[h.match.field] ?? h.match.field} "${h.match.text}"`);
  if (h.deprecated) parts.push('deprecated');
  return {
    id: h.iri,
    title,
    highlight: onLabel && title === h.match.text ? h.match.highlight : [],
    subtitle: parts.join(' · '),
    tag: KIND_NAME[h.kinds[0]] ?? h.kinds[0],
    href: entityHref(base, ds, h),
  };
}

/** The shell's provider: the open dataset's entities, eight at a time. */
export function entityProvider(
  base: string,
  prefixes: (ds: string) => PrefixMap = () => WELL_KNOWN,
): LauncherProvider {
  return {
    id: 'entities',
    label: ({ ds }) => (ds ? `Entities in ${ds}` : 'Entities'),
    order: 10,
    active: ({ ds }) => !!ds,
    async search(q, { ds, signal }) {
      if (!ds || !q.trim()) return [];
      const r = await searchEntities(ds, q.trim(), { limit: 8, signal });
      return r.results.map((h) => entityItem(base, ds, h, prefixes(ds)));
    },
  };
}
