// Import resolution in the ontology editor (docs/specs/U12-ontology-workbench/SPEC.md,
// "Import resolution"): the client of the dataset's import map and of its catalog upload
// and fetch actions, and the tree the Imports tab draws from the summary's closure.
import { request } from './api';
import { commitMessageHeader } from './ontology';

const enc = encodeURIComponent;

/** One `owl:imports` triple of a graph read, as the summary's `closure` lists it. */
export type ImportEdge = {
  from: string;
  iri: string;
  graphs: string[];
  via: 'map' | 'graph' | 'resource' | null;
  cycle: boolean;
};

/** One entry of the import map. */
export type ImportEntry = {
  iri: string;
  graph: string;
  source: 'manual' | 'catalog' | 'fetch';
  file?: string;
  location?: string;
  branch?: string;
  commit?: number;
  etag?: string;
  digest?: string;
  fetched?: string;
};

export type ImportMap = {
  dataset: string;
  revision: string;
  admin: boolean;
  maxEntries: number;
  entries: ImportEntry[];
};

export type CatalogAnswer = {
  loaded: { graph: string; file: string; triples: number }[];
  entries: ImportEntry[];
  skipped: { entry: string; reason: string }[];
  changed: boolean;
  revision: string;
};

export type FetchAnswer = {
  entry: ImportEntry;
  triples: number;
  changed: boolean;
  revision: string;
};

/** `GET /$/ontology/{ds}/imports`: the map as the caller may see it. */
export async function importMap(target: string, signal?: AbortSignal): Promise<ImportMap> {
  const res = await request(`/$/ontology/${enc(target)}/imports`, {
    headers: { Accept: 'application/json' },
    cache: 'no-store',
    signal,
  });
  return (await res.json()) as ImportMap;
}

/** `PATCH /$/ontology/{ds}/imports`: set and remove entries at the map's revision. */
export async function patchImportMap(
  target: string,
  change: { set?: Record<string, string>; remove?: string[] },
  revision: string | null,
): Promise<ImportMap> {
  const headers: Record<string, string> = {
    Accept: 'application/json',
    'Content-Type': 'application/json',
  };
  if (revision) headers['If-Match'] = revision;
  const res = await request(`/$/ontology/${enc(target)}/imports`, {
    method: 'PATCH',
    headers,
    body: JSON.stringify(change),
  });
  return (await res.json()) as ImportMap;
}

/** The headers of an import action: the commit message and the summary's tag. */
function actionHeaders(o: { etag?: string | null; message?: string }): Record<string, string> {
  const h: Record<string, string> = { Accept: 'application/json' };
  if (o.etag) h['If-Match'] = o.etag;
  if (o.message) h['Sparkles-Commit-Message'] = commitMessageHeader(o.message);
  return h;
}

/**
 * `POST /$/ontology/{ds}/imports/catalog`: an XML catalog and the files it names, each part
 * keyed by the path the catalog gives (the file's relative path in a chosen folder, or its
 * name).
 */
export async function uploadCatalog(
  target: string,
  catalog: File,
  files: File[],
  o: { etag?: string | null; message?: string } = {},
): Promise<CatalogAnswer> {
  const body = new FormData();
  body.append('catalog', catalog, catalog.name);
  for (const f of files) body.append(partKey(f), f, f.name);
  const res = await request(`/$/ontology/${enc(target)}/imports/catalog?receipt=true`, {
    method: 'POST',
    headers: actionHeaders(o),
    body,
  });
  return (await res.json()) as CatalogAnswer;
}

/**
 * The key of a file's part: its path below the chosen folder, which is what a catalog in
 * that folder names, or its name.
 */
export function partKey(f: { name: string; webkitRelativePath?: string }): string {
  const rel = f.webkitRelativePath ?? '';
  const i = rel.indexOf('/');
  return i >= 0 ? rel.slice(i + 1) : f.name;
}

/** `POST /$/ontology/{ds}/imports/fetch`: fetch an import into its graph. */
export async function fetchImport(
  target: string,
  req: { iri: string; location?: string; graph?: string },
  o: { etag?: string | null; message?: string } = {},
): Promise<FetchAnswer> {
  const res = await request(`/$/ontology/${enc(target)}/imports/fetch?receipt=true`, {
    method: 'POST',
    headers: { ...actionHeaders(o), 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  });
  return (await res.json()) as FetchAnswer;
}

/** A node of the closure tree: an import and what it leads to. */
export type ClosureNode = {
  edge: ImportEdge;
  /** the imports of the graphs this import resolved to that were not drawn earlier */
  children: ClosureNode[];
  /** the graphs were drawn higher in the tree, so their imports are not repeated */
  repeated: boolean;
};

/**
 * The closure as a tree from `root`: each graph's imports in the order the summary lists
 * them. A graph already drawn ends its branch, so a cycle shows once, where it closes.
 */
export function closureTree(root: string, edges: readonly ImportEdge[]): ClosureNode[] {
  const from = new Map<string, ImportEdge[]>();
  for (const e of edges) {
    const l = from.get(e.from) ?? [];
    l.push(e);
    from.set(e.from, l);
  }
  const drawn = new Set<string>([root]);
  const grow = (g: string): ClosureNode[] =>
    (from.get(g) ?? []).map((edge) => {
      const fresh = edge.graphs.filter((x) => !drawn.has(x));
      const repeated = edge.graphs.length > 0 && fresh.length === 0;
      for (const x of fresh) drawn.add(x);
      const children = fresh.flatMap(grow);
      return { edge, children, repeated };
    });
  return grow(root);
}

/** The counts the tab's heading shows. */
export function closureCounts(edges: readonly ImportEdge[]): {
  imports: number;
  unresolved: number;
  cycles: number;
} {
  const iris = new Set(edges.map((e) => e.iri));
  const unresolved = new Set(edges.filter((e) => e.graphs.length === 0).map((e) => e.iri));
  return {
    imports: iris.size,
    unresolved: unresolved.size,
    cycles: edges.filter((e) => e.cycle).length,
  };
}
