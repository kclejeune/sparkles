// The entity search of `GET /$/ontology/{ds}/search` (docs/API.md, "Entity search"), and
// the launcher provider that shows its hits in the top bar.

import { request } from './api';
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
  opts: { limit?: number; kind?: EntitySearchKind[]; signal?: AbortSignal } = {},
): Promise<EntitySearch> {
  const p = new URLSearchParams({ q, limit: String(opts.limit ?? 10) });
  if (opts.kind?.length) p.set('kind', opts.kind.join(','));
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
 * Where a hit opens: a concept, scheme or collection in the thesaurus editor, a class or
 * property in the ontology editor, and anything else in Explore. Item 616's browser pages
 * replace the first two.
 */
export function entityHref(base: string, ds: string, hit: Pick<EntityHit, 'iri' | 'kinds'>) {
  const q = `?ds=${encodeURIComponent(ds)}&iri=${encodeURIComponent(hit.iri)}`;
  const k = hit.kinds;
  if (k.some((x) => x === 'concept' || x === 'conceptScheme' || x === 'collection'))
    return `${base}/thesaurus${q}`;
  if (k.some((x) => x === 'class' || x.endsWith('Property') || x === 'datatype'))
    return `${base}/ontology${q}`;
  return `${base}/explore${q}`;
}

/** A hit as a launcher row: the label with its match, and what matched when not the label. */
export function entityItem(base: string, ds: string, h: EntityHit): LauncherItem {
  const onLabel = h.match.field === 'label';
  const title = h.label ?? h.shortForm ?? h.iri;
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
export function entityProvider(base: string): LauncherProvider {
  return {
    id: 'entities',
    label: ({ ds }) => (ds ? `Entities in ${ds}` : 'Entities'),
    order: 10,
    active: ({ ds }) => !!ds,
    async search(q, { ds, signal }) {
      if (!ds || !q.trim()) return [];
      const r = await searchEntities(ds, q.trim(), { limit: 8, signal });
      return r.results.map((h) => entityItem(base, ds, h));
    },
  };
}
