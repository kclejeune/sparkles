// Cloning a dataset (`POST /$/datasets/{ds}/clone`): the request the Clone dialog sends
// and how a finished clone task says the copy was made.

import type { DatasetOrigin, Task } from './api';

/** The graph reasoning writes its inferences to. */
export const INFERRED_GRAPH = 'urn:x-sparkles:inferred';

export type CloneType = 'persistent' | 'mem';
export type CloneMode = 'auto' | 'link' | 'rebuild';
export type CloneMethod = 'link' | 'reflink' | 'copy' | 'rebuild';

export type CloneOptions = {
  /** `persistent` (the default) or an in-memory dataset. */
  type?: CloneType;
  /** Copy the inferred graph and the reasoning status (the default). */
  inferences?: 'copy' | 'drop';
  /** How a persistent clone gets its index files; `auto` by default. */
  mode?: CloneMode;
  /** Only these graphs: `default`, graph IRIs or IRI patterns with `*`. All when empty. */
  graphs?: string[];
};

/** A finished clone task's `detail`. */
export type CloneDetail = {
  method: CloneMethod;
  rebuildReason: string | null;
  type: CloneType;
  quads: number;
  graphs: number;
  bytes: number;
  millis: number;
};

/** The JSON body of a clone request. Defaults are left out, and `mode` is sent only for a
 *  persistent clone, which is the only kind that can share the source's index files. */
export function cloneBody(name: string, opts: CloneOptions = {}): Record<string, unknown> {
  const body: Record<string, unknown> = { name, inferences: opts.inferences ?? 'copy' };
  const type = opts.type ?? 'persistent';
  if (type !== 'persistent') body.type = type;
  if (type === 'persistent' && opts.mode && opts.mode !== 'auto') body.mode = opts.mode;
  if (opts.graphs?.length) body.graphs = opts.graphs;
  return body;
}

/** Graph names typed one per line or separated by spaces, without blanks and repeats. */
export function parseGraphs(text: string): string[] {
  return [...new Set(text.split(/\s+/).filter(Boolean))];
}

/** The graphs a partial clone names: the chosen and typed ones, and the inferred graph
 *  when inferences are copied (a partial clone keeps them only when it names that graph). */
export function selectedGraphs(chosen: string[], typed: string, inferences: boolean): string[] {
  const all = [...chosen, ...parseGraphs(typed)];
  if (inferences && all.length) all.push(INFERRED_GRAPH);
  return [...new Set(all)];
}

/** A clone task's detail, when the server sent one. */
export function cloneDetail(t: Pick<Task, 'kind' | 'detail'>): CloneDetail | null {
  if (t.kind !== 'clone') return null;
  const d = t.detail as Partial<CloneDetail> | null | undefined;
  return d && typeof d === 'object' && typeof d.method === 'string' ? (d as CloneDetail) : null;
}

/** The end of a clone's "cloned from … at commit …" line: what it left out and how its
 *  index was made (`origin.json`). */
export function originSummary(o: Pick<DatasetOrigin, 'inferences' | 'graphs' | 'method'>): string {
  const parts: string[] = [];
  if (o.inferences === 'drop') parts.push('without inferences');
  if (o.graphs?.length) parts.push(`only ${o.graphs.join(', ')}`);
  if (o.method) parts.push(o.method === 'rebuild' ? 'index rebuilt' : `index by ${o.method}`);
  return parts.map((p) => `, ${p}`).join('');
}

/** How the copy was made, in a few words. */
export function cloneMethodText(d: Pick<CloneDetail, 'method' | 'rebuildReason'>): string {
  switch (d.method) {
    case 'link':
      return 'hard-linked the source’s index files';
    case 'reflink':
      return 'shared the source’s index files by reflink';
    case 'copy':
      return 'copied the source’s index files';
    default:
      return d.rebuildReason ? `rebuilt the index: ${d.rebuildReason}` : 'rebuilt the index';
  }
}
