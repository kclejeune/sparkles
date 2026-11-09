// Every address of the pages whose tabs are routes or parameters (ROADMAP item 633): the
// Admin pages, Similar's Search and Map, the Browse reader and the Workbench editors. The
// rail, the palette, in-app links and the tests build these addresses here, so moving a
// page or splitting a merged one changes this module rather than its callers.

import { base } from '$app/paths';

export type Params = Record<string, string | null | undefined>;

const q = (params: Params) => {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v) p.set(k, v);
  const s = p.toString();
  return s ? `?${s}` : '';
};

/** The Workbench's editors. */
export type Editor = 'ontology' | 'thesaurus' | 'shapes';

/**
 * The Ontology editor's tabs: the tree (no `tab`), DL Query, the header, the history and the
 * discussions.
 * The tree's entity kind is `kind` (no `kind`: classes).
 */
export const ONTOLOGY_TABS = [
  'tree',
  'dlquery',
  'header',
  'history',
  'discussions',
  'bulk',
] as const;
export type OntologyTab = (typeof ONTOLOGY_TABS)[number];
/** The Thesaurus editor's tabs: the tree (no `tab`), the history and the validation report. */
export const THESAURUS_TABS = ['tree', 'history', 'validation'] as const;
export type ThesaurusTab = (typeof THESAURUS_TABS)[number];

/** The reader's page of `iri` in `ds`, or the dataset's landing page without one. */
export function browsePath(b: string, ds: string, params: Params = {}): string {
  return `${b}/browse${q({ ds, ...params })}`;
}

/** An editor of the Workbench, on the tab `params.tab` (the tree when absent). */
export function workbenchPath(b: string, editor: Editor, ds: string, params: Params = {}): string {
  const { tab, ...rest } = params;
  return `${b}/workbench/${editor}${q({ ds, tab: tab === 'tree' ? null : tab, ...rest })}`;
}

/** The editor of an entity with these kinds: the thesaurus for SKOS, the ontology otherwise. */
export function editorOf(kinds: readonly string[]): Editor {
  return kinds.some((k) => k === 'concept' || k === 'conceptScheme' || k === 'collection')
    ? 'thesaurus'
    : 'ontology';
}

export const routes = {
  /** The app's base path, such as `/ui`. */
  get base() {
    return base;
  },
  adminServer: () => `${base}/admin/server`,
  adminBackups: (params: Params = {}) => `${base}/admin/backups${q(params)}`,
  adminTokens: () => `${base}/admin/tokens`,
  /** Similar's Search page. */
  similar: (ds: string | null, params: Params = {}) => `${base}/similar${q({ ds, ...params })}`,
  /** Similar's Map page: the embedding map of a vector index. */
  similarMap: (ds: string | null, params: Params = {}) =>
    `${base}/similar/map${q({ ds, ...params })}`,
  /** The Browse reader. */
  browse: (ds: string, params: Params = {}) => browsePath(base, ds, params),
  /** The Workbench's Ontology editor. */
  ontology: (ds: string, params: Params = {}) => workbenchPath(base, 'ontology', ds, params),
  /** The Workbench's Thesaurus editor. */
  thesaurus: (ds: string, params: Params = {}) => workbenchPath(base, 'thesaurus', ds, params),
  shapes: (ds: string, params: Params = {}) => workbenchPath(base, 'shapes', ds, params),
  workbench: (editor: Editor, ds: string, params: Params = {}) =>
    workbenchPath(base, editor, ds, params),
};
