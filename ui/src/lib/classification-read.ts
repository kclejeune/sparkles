// Read-only kept classification pages and the deliberate one-request frame action.
import { request } from './api';
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
  incomplete?: boolean;
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
export type ClassificationView = {
  target: string;
  selection: Selection;
  iri: string;
  commit: number;
  mode: 'lenient' | 'strict';
  languages: string[];
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
function scope(view: ClassificationView) {
  return new URLSearchParams({
    graph: view.selection.graph ?? 'default',
    imports: String(view.selection.imports),
    mode: view.mode,
    at: `commit:${view.commit}`,
    lang: view.languages.join(','),
  });
}
export async function readKeptClassification(
  view: ClassificationView,
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
  if (query.part !== 'sources' && query.part !== 'excluded' && query.part !== 'roots')
    params.set('about', query.about ?? view.iri);
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
