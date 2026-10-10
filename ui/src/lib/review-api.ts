// The review routes of C18 Phase 3: the inbox of §8.9, the review of one branch
// (§7.10), the reviewer's actions, and the ingest settings and profiles of §7.4.

import { json, request } from './api';

const enc = encodeURIComponent;

const send = (method: string, body: unknown): RequestInit => ({
  method,
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

/** A signal's outcome: `none` when there is nothing to check (no span). */
export type Signal = 'pass' | 'fail' | 'none' | 'unchecked';

export type Signals = {
  span: Signal;
  link: Signal;
  guard: Signal;
  corroboration: Signal;
};

/** A fact as the review routes list it: terms in N-Triples form. */
export type ReviewFact = {
  s: string;
  p: string;
  o: string;
  graph: string;
  shown: { s: string; p: string; o: string; graph: string };
  sLabel?: string;
  oLabel?: string;
  status: 'unreviewed' | 'proposed' | 'reviewed';
  reifiers: string[];
  time?: string;
  confidence?: string;
  quote?: string;
  by?: string;
  agent?: string;
  span?: { rendition: string; start: number; end: number };
  signals?: Signals;
  passes?: boolean;
  candidates?: { iri: string; shown: string; label?: string }[];
  notes?: string[];
};

/** A fact named by an action. */
export type FactRef = { s: string; p: string; o: string; graph: string };

export const factRef = (f: FactRef): FactRef => ({ s: f.s, p: f.p, o: f.o, graph: f.graph });

export type InboxSession = {
  graph: string;
  shown: string;
  by: string[];
  first?: string;
  last?: string;
  facts: ReviewFact[];
};

export type InboxBranch = {
  name: string;
  kind: 'ingest' | 'review' | 'inbox' | 'consolidation' | 'proposal';
  ahead: number;
  behind: number;
  created?: string;
  modified?: string;
  note?: string;
  creator?: string;
  facts?: number;
  retracts?: number;
};

/** `GET /$/memory/{ds}/inbox`. */
export type Inbox = {
  dataset: string;
  commit?: number;
  agentGraphs?: string[];
  target?: string;
  sessions: InboxSession[];
  branches: InboxBranch[];
  open: number;
  truncated: boolean;
  prefixes?: Record<string, string>;
};

export const inbox = (ds: string, signal?: AbortSignal) =>
  json<Inbox>(`/$/memory/${enc(ds)}/inbox`, { signal, cache: 'no-store' });

export type ReviewEntity = {
  iri: string;
  shown: string;
  label?: string;
  types: string[];
  candidates: { iri: string; shown: string; label?: string }[];
};

export type ReviewSource = {
  rendition: string;
  source?: string;
  title?: string;
  format?: string;
  length: number;
  text?: string;
  textOmitted?: boolean;
};

/** `GET /$/memory/{ds}/review/{branch}`. */
export type BranchReview = {
  dataset: string;
  branch: string;
  kind?: InboxBranch['kind'];
  base?: number;
  head?: number;
  ahead?: number;
  behind?: number;
  note?: string;
  creator?: string;
  facts: ReviewFact[];
  retracts: ReviewFact[];
  rejected?: number;
  entities: ReviewEntity[];
  sources: ReviewSource[];
  prefixes?: Record<string, string>;
};

export const branchReview = (ds: string, branch: string, signal?: AbortSignal) =>
  json<BranchReview>(`/$/memory/${enc(ds)}/review/${enc(branch)}`, {
    signal,
    cache: 'no-store',
  });

export type PromoteResult = {
  dataset: string;
  branch: string;
  target: string;
  promoted: number;
  commit?: number;
  committed: boolean;
};

export const promote = (
  ds: string,
  facts: FactRef[],
  opts: { target?: string; branch?: string; message?: string } = {},
) =>
  json<PromoteResult>(
    `/$/memory/${enc(ds)}/promote`,
    send('POST', { facts: facts.map(factRef), ...opts }),
  );

export type RejectResult = {
  dataset: string;
  branch: string;
  rejected: number;
  commits: unknown[];
};

export const reject = (
  ds: string,
  facts: FactRef[],
  opts: { branch?: string; reason?: string } = {},
) =>
  json<RejectResult>(
    `/$/memory/${enc(ds)}/reject`,
    send('POST', { facts: facts.map(factRef), ...opts }),
  );

export type WriteResult = {
  dataset: string;
  branch?: string;
  committed?: boolean;
  commit?: number;
  inserted?: number;
  deleted?: number;
};

/** **Use existing**: `from` becomes `to` on the branch. IRIs without angle brackets. */
export const relink = (ds: string, branch: string, from: string, to: string) =>
  json<WriteResult>(`/$/memory/${enc(ds)}/relink`, send('POST', { branch, from, to }));

/** **Edit value**: the fact with a new object. */
export const editFact = (ds: string, fact: FactRef, o: string, branch?: string) =>
  json<WriteResult>(
    `/$/memory/${enc(ds)}/edit`,
    send('POST', { fact: factRef(fact), o, ...(branch ? { branch } : {}) }),
  );

// --- ingest settings --------------------------------------------------------------------

/** An ingest profile (§7.3): lists left out mean the whole schema. */
export type IngestProfile = {
  classes?: string[];
  predicates?: string[];
  shapes?: string;
  labelPredicate?: string;
  language?: string;
  vocabulary?: string;
};

export type IngestProfiles = {
  dataset: string;
  keepText: boolean;
  profiles: Record<string, IngestProfile>;
};

export const ingestProfiles = (ds: string, signal?: AbortSignal) =>
  json<IngestProfiles>(`/$/ingest/${enc(ds)}/profiles`, { signal, cache: 'no-store' });

export const putKeepText = (ds: string, keepText: boolean) =>
  json<{ keepText: boolean }>(`/$/ingest/${enc(ds)}/settings`, send('PUT', { keepText }));

export const putProfile = (ds: string, name: string, p: IngestProfile) =>
  json<IngestProfile>(`/$/ingest/${enc(ds)}/profiles/${enc(name)}`, send('PUT', p));

export async function deleteProfile(ds: string, name: string): Promise<void> {
  await request(`/$/ingest/${enc(ds)}/profiles/${enc(name)}`, { method: 'DELETE' });
}
