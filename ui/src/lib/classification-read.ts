// Read-only kept classification pages and the deliberate one-request frame action.
import { ApiError, request } from './api';
import {
  formatTimeoutSeconds,
  type AxiomItem,
  type ClassificationExpansion,
  type ClassifyOptions,
  type Selection,
} from './ontology';

export type { ClassificationExpansion, ClassifyOptions };

export const CLASSIFICATION_PAGE_SIZE = 100;
export const CLASSIFICATION_DISPLAY_CAP = 1000;
export type ClassificationFramePart = 'parents' | 'equivalents' | 'sources' | 'excluded';
export type ClassificationPart = ClassificationFramePart | 'roots' | 'children';
export type ClassificationName = {
  iri: string;
  label: string | null;
  labelLang: string | null;
  equivalenceCount?: number;
  children?: number;
  parents?: number;
};
export type ClassificationSourceRow =
  | { kind: 'requestedGraph'; ordinal: number; graph: string }
  | { kind: 'resolvedGraph'; ordinal: number; graph: string; requested: boolean }
  | { kind: 'import'; ordinal: number; iri: string; location: string | null; graph: string | null }
  | { kind: 'warning'; ordinal: number; message: string };
export type ClassificationExcluded = AxiomItem & {
  gap:
    | { kind: 'omitted'; rule: string; message: string }
    | { kind: 'partiallyUsed'; reason: string };
};
export type ClassificationRow =
  | ClassificationName
  | ClassificationSourceRow
  | ClassificationExcluded;
export type ClassificationResult = {
  resultId: string;
  classifiedCommit: number;
  boundStoreId: string;
  familyId: string;
  inputFingerprint: string;
  sourceFingerprint: string;
  contentFingerprint: string;
  imports: 'dataset' | 'none';
  mode: 'lenient' | 'strict';
  assertions: boolean;
  elOnly: boolean;
  partialRequested: boolean;
  expansion: 'cautious' | 'eager';
  sourceCounts: {
    requestedGraphs: number;
    resolvedGraphs: number;
    imports: number;
    warnings: number;
  };
  capabilities: string[];
  justifications: 'unavailable';
};
export type ClassificationOutcome = {
  tier: 'el' | 'horn' | 'disjunctive';
  partial: boolean;
  complete: boolean;
  consistency: 'consistent' | 'inconsistent' | 'unknown';
  excludedCount: number;
  incompleteCount: number;
  unmappedCount: number;
};
export type ClassificationPage = {
  part: ClassificationPart;
  about: string | null;
  offset: number;
  limit: number;
  total: number;
  items: ClassificationRow[];
  nextOffset: number | null;
};
export type KeptClassification = {
  commit: number;
  status: 'available' | 'running' | 'mismatch' | 'unavailable';
  reason: string | null;
  result: ClassificationResult | null;
  outcome: ClassificationOutcome | null;
  entity: {
    iri: string;
    present: boolean;
    unsatisfiable: boolean | null;
    incomplete: boolean | null;
    representative?: string | null;
    equivalenceCount?: number | null;
    children?: number | null;
    parents?: number | null;
  } | null;
  activity: { kind: string; commit: number } | null;
  page: ClassificationPage | null;
  staleCommit: number | null;
};
export type ClassificationScope = {
  target: string;
  selection: Selection;
  commit: number;
  mode: 'lenient' | 'strict';
  languages: string[];
};
export type ClassificationView = ClassificationScope & { iri: string };
export type KeptSelection = Omit<KeptClassification, 'page'>;
export type KeptEntity = KeptSelection & {
  source: 'keptClassification';
  covers: string[];
  parents: ClassificationPage | null;
  children: ClassificationPage | null;
  equivalents: ClassificationPage | null;
};
export type KeptAxioms = KeptSelection & {
  source: 'keptClassification';
  axioms: AxiomItem[] | null;
  offset: number;
  limit: number;
  total: number | null;
  next: number | null;
};
export type ProjectedClassification = {
  commit: number;
  frame: {
    iri: string;
    result: ClassificationResult;
    outcome: ClassificationOutcome;
    unsatisfiable: boolean;
    incomplete: boolean;
    parents: { items: ClassificationName[]; total: number; truncated: boolean; limit: number };
    equivalents: { items: ClassificationName[]; total: number; truncated: boolean; limit: number };
    sources: { items: ClassificationSourceRow[]; total: number; truncated: boolean };
    excluded: { items: ClassificationExcluded[]; total: number; truncated: boolean; limit: number };
    retention: { kept: boolean; resultId: string | null };
  };
};
function scope(view: ClassificationScope) {
  return new URLSearchParams({
    graph: view.selection.graph ?? 'default',
    imports: String(view.selection.imports),
    mode: view.mode,
    at: `commit:${view.commit}`,
    lang: view.languages.join(','),
  });
}
export async function readKeptClassification(
  view: ClassificationScope & { iri?: string },
  query: {
    part?: ClassificationPart;
    offset?: number;
    resultId?: string;
    about?: string;
    limit?: number;
  } = {},
  signal?: AbortSignal,
): Promise<KeptClassification> {
  const params = scope(view);
  params.set('part', query.part ?? 'status');
  const about = query.about ?? view.iri;
  if (about && query.part !== 'sources' && query.part !== 'excluded' && query.part !== 'roots')
    params.set('about', about);
  params.set('limit', String(query.limit ?? CLASSIFICATION_PAGE_SIZE));
  if (query.resultId) params.set('resultId', query.resultId);
  if (query.offset != null) params.set('offset', String(query.offset));
  const res = await request(
    `/$/ontology/${encodeURIComponent(view.target)}/classification?${params}`,
    {
      headers: { Accept: 'application/json' },
      cache: 'no-store',
      signal,
    },
  );
  return await res.json();
}
/** The entity route requires an asserted named class. A taxonomy may also have built-in nodes. */
export async function readKeptEntity(
  view: ClassificationView,
  resultId: string | undefined,
  signal: AbortSignal,
): Promise<KeptEntity | null> {
  const params = scope(view);
  params.set('classification', 'kept');
  params.set('iri', view.iri);
  params.set('kind', 'Class');
  params.set('limit', String(CLASSIFICATION_PAGE_SIZE));
  if (resultId) params.set('resultId', resultId);
  try {
    const res = await request(`/$/ontology/${encodeURIComponent(view.target)}/entity?${params}`, {
      headers: { Accept: 'application/json' },
      cache: 'no-store',
      signal,
    });
    const { commit, iri, classification } = (await res.json()) as {
      commit: number;
      iri: string;
      classification: KeptEntity;
    };
    if (
      commit !== view.commit ||
      iri !== view.iri ||
      classification?.source !== 'keptClassification' ||
      !classification.covers.includes('namedClass')
    )
      throw new Error('The kept entity read changed the displayed source or entity.');
    return classification;
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export async function readKeptAxioms(
  view: ClassificationView,
  resultId: string,
  offset: number,
  signal: AbortSignal,
): Promise<KeptAxioms> {
  const params = scope(view);
  params.delete('lang');
  params.set('classification', 'kept');
  params.set('about', view.iri);
  params.set('limit', String(CLASSIFICATION_PAGE_SIZE));
  params.set('offset', String(offset));
  params.set('resultId', resultId);
  const res = await request(`/$/ontology/${encodeURIComponent(view.target)}/axioms?${params}`, {
    headers: { Accept: 'application/json' },
    cache: 'no-store',
    signal,
  });
  const reply: KeptAxioms = await res.json();
  if (reply.source !== 'keptClassification')
    throw new Error('The axiom listing did not return a kept classification source.');
  return reply;
}

export function validateKeptSelection(reply: KeptSelection, view: ClassificationScope) {
  if (reply.commit !== view.commit) throw new Error('The kept read changed the displayed commit.');
  if (reply.status !== 'available') return;
  const { result, outcome } = reply;
  if (
    !result ||
    !outcome ||
    result.classifiedCommit !== view.commit ||
    result.mode !== view.mode ||
    result.imports !== (view.selection.imports ? 'dataset' : 'none') ||
    result.assertions ||
    result.elOnly
  )
    throw new Error('The kept result has no matching source and outcome.');
}

export function keptAvailability(reply: KeptSelection) {
  if (reply.status === 'running') return 'Classification is running for this source.';
  if (reply.reason === 'unsupportedScope') return 'This view does not share a kept classification.';
  if (reply.staleCommit != null)
    return `The kept classification is stale. It covers commit ${reply.staleCommit}.`;
  if (reply.status === 'mismatch')
    return 'The kept classification does not match this source or result pin.';
  return 'No kept classification is available for this source.';
}

/** Only a deliberate Classify action calls this POST. Pagination always uses GET. */
export async function projectClassification(
  view: ClassificationView,
  signal?: AbortSignal,
  options?: ClassifyOptions,
): Promise<ProjectedClassification> {
  const params = new URLSearchParams({ at: `commit:${view.commit}` });
  const timeoutSec = formatTimeoutSeconds(options?.timeoutMs);
  if (timeoutSec) params.set('timeout', timeoutSec);
  const body: Record<string, unknown> = {
    graph: view.selection.graph ?? 'default',
    imports: view.selection.imports,
    mode: view.mode,
    projection: 'frame',
    about: view.iri,
    limit: CLASSIFICATION_PAGE_SIZE,
    lang: view.languages,
  };
  if (options?.partial) body.partial = true;
  if (options?.expansion) body.expansion = options.expansion;
  const res = await request(`/$/ontology/${encodeURIComponent(view.target)}/classify?${params}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify(body),
    signal,
  });
  return await res.json();
}
