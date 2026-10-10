// The HTTP calls of C18 Phase 1: checking a query, diagnosing an empty result, recalling
// memory, the memory settings and the suggested examples of stored queries.

import { json, request } from './api';

const enc = encodeURIComponent;

const post = (body: unknown, signal?: AbortSignal): RequestInit => ({
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
  signal,
});

/** A suggested term of a check issue. */
export type CheckSuggestion = { term: string; label?: string; count: number; why: string };

export type CheckIssue = {
  code: string;
  severity: 'error' | 'warning';
  message: string;
  term?: string;
  line?: number;
  column?: number;
  suggestions?: CheckSuggestion[];
};

/** A constant IRI of a checked query (`terms: true`), in compact form. */
export type CheckTerm = {
  term: string;
  iri: string;
  kind: 'class' | 'property' | 'entity';
  label?: string;
  /** Triples of a property, instances of a class. */
  count?: number;
  /** An entity's classes, compact. */
  types?: string[];
  occurs: boolean;
};

/** `POST /{ds}/check`: the C17 `check_query` result. */
export type CheckResult = {
  dataset: string;
  commit?: number;
  ok: boolean;
  issues: CheckIssue[];
  estimatedRows?: number;
  terms?: CheckTerm[];
  prefixes: Record<string, string>;
};

export type CheckOptions = {
  explain?: boolean;
  terms?: boolean;
  maxSuggestions?: number;
  reasoning?: boolean;
  at?: string;
  atCommit?: number;
  timeoutSeconds?: number;
  signal?: AbortSignal;
};

/** Check a query against the caller's view without running it. `ds` may name a branch. */
export function checkQuery(ds: string, query: string, opts: CheckOptions = {}) {
  const { signal, ...rest } = opts;
  return json<CheckResult>(`/${enc(ds)}/check`, post({ query, ...rest }, signal));
}

/** `POST /{ds}/sparql/diagnose`: why a query has no solutions (MCP `why_empty`). */
export type Diagnosis = {
  dataset: string;
  commit?: number;
  empty: boolean;
  first?: {
    kind: 'pattern' | 'join' | 'filter';
    text: string;
    line?: number;
    column?: number;
    constants: { term: string; occurs: boolean }[];
    issues: CheckIssue[];
  };
  steps: { kind: string; text: string; solutions: boolean | null }[];
  complete: boolean;
  message: string;
  prefixes: Record<string, string>;
};

export function diagnoseQuery(
  ds: string,
  query: string,
  opts: { reasoning?: boolean; at?: string; atCommit?: number; signal?: AbortSignal } = {},
) {
  const { signal, ...rest } = opts;
  return json<Diagnosis>(`/${enc(ds)}/sparql/diagnose`, post({ query, ...rest }, signal));
}

export type FactStatus = 'reviewed' | 'unreviewed';

export type RecallFact = {
  s: string;
  p: string;
  o: string;
  citation: number | string;
  status?: FactStatus;
};

export type RecallEntity = {
  iri: string;
  label?: string;
  types: string[];
  seed?: number;
  hop: number;
  facts: RecallFact[];
};

export type RecallCitation = {
  id: number | string;
  graph: string;
  reifier?: string;
  source?: string;
  at?: string;
  by?: string;
  confidence?: number;
  quote?: string;
  status?: FactStatus;
};

export type RecallSuperseded = {
  s: string;
  p: string;
  o: string;
  graph: string;
  reifier: string;
  at?: string;
  invalidatedAt?: string;
  replacedBy?: string;
};

export type RecallConflict = {
  s: string;
  p: string;
  values: { o: string; citation: number | string }[];
};

/** `POST /{ds}/recall`: C17's recall result in JSON. */
export type RecallResult = {
  dataset: string;
  commit?: number;
  entities: RecallEntity[];
  citations: RecallCitation[];
  superseded?: RecallSuperseded[];
  conflicts: RecallConflict[];
  truncated: boolean;
  prefixes: Record<string, string>;
};

export type RecallRequest = {
  query?: string;
  seeds?: string[];
  types?: string[];
  graphs?: string[];
  hops?: number;
  seedLimit?: number;
  maxTriples?: number;
  maxBytes?: number;
  includeSuperseded?: boolean;
  statuses?: FactStatus[];
  unreviewedWeight?: number;
  reasoning?: boolean;
  at?: string;
  atCommit?: number;
  timeoutSeconds?: number;
};

export function recall(ds: string, req: RecallRequest, signal?: AbortSignal) {
  return json<RecallResult>(`/${enc(ds)}/recall`, post(req, signal));
}

/** `GET /$/memory/{ds}`: which graphs hold unreviewed agent memory. */
export type MemorySettings = {
  agentGraphs: string[];
  consolidatedGraph?: string;
  agents: Record<string, { conversationFacts: 'immediate' | 'review' }>;
};

export const memorySettings = (ds: string, signal?: AbortSignal) =>
  json<MemorySettings>(`/$/memory/${enc(ds)}`, { signal, cache: 'no-store' });

/** A question and query a person suggested as a stored-query example. */
export type Suggestion = {
  id: string;
  question: string;
  query: string;
  explanation?: string;
  by: string;
  at: string;
};

export const suggestExample = (
  ds: string,
  s: { question: string; query: string; explanation?: string },
) => json<Suggestion>(`/$/queries/${enc(ds)}/suggestions`, post(s));

export async function suggestions(ds: string, signal?: AbortSignal): Promise<Suggestion[]> {
  const body = await json<{ suggestions: Suggestion[] }>(`/$/queries/${enc(ds)}/suggestions`, {
    signal,
    cache: 'no-store',
  });
  return body?.suggestions ?? [];
}

export async function deleteSuggestion(ds: string, id: string): Promise<void> {
  await request(`/$/queries/${enc(ds)}/suggestions?id=${enc(id)}`, { method: 'DELETE' });
}
