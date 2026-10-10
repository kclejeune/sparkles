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
  /** each page's number and start offset, for a converted PDF */
  pages?: { page: number; start: number }[];
  ocrPages?: number[];
  omittedPages?: number[];
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

export type IngestSettings = {
  keepText?: boolean;
  /** `null` restores the default */
  confirmTokens?: number | null;
  autoConfidence?: number | null;
};

export const putIngestSettings = (ds: string, s: IngestSettings) =>
  json<IngestSettings & { dataset: string }>(`/$/ingest/${enc(ds)}/settings`, send('PUT', s));

// --- ingestion tasks (C18 Phase 4) ------------------------------------------------------

export type IngestMode = 'branch' | 'preview' | 'auto';

export type IngestStatus =
  | 'queued'
  | 'converting'
  | 'registering'
  | 'awaiting-confirmation'
  | 'extracting'
  | 'linking'
  | 'writing'
  | 'awaiting-approval'
  | 'done'
  | 'failed'
  | 'cancelled';

/** A page that needs OCR, with pdf-inspector's reasons. */
export type PageReason = { page: number; reasons: string[] };

export type IngestEstimate = {
  chunks: number;
  inputTokens: number;
  outputTokens: number;
  tokens: number;
  pair?: { provider: string; model: string };
  estimatedCost?: number;
  threshold?: number;
  needsConfirmation?: boolean;
};

export type IngestError = {
  code: string;
  message: string;
  status?: number;
  pages?: PageReason[];
  fonts?: string[];
  estimate?: IngestEstimate;
};

export type IngestResult = {
  outcome: string;
  mode?: IngestMode;
  format?: string;
  source?: string;
  graph?: string;
  rendition?: string;
  branch?: string;
  review?: string;
  length?: number;
  chunks?: number;
  proposed?: number;
  extracted?: number;
  failed?: { code: string; message?: string; quote?: string }[];
  entities?: { linked: number; new: number; ambiguous: number };
  pages?: { page: number; start: number }[];
  ocrPages?: number[];
  omittedPages?: number[];
  notes?: string[];
  autoFallback?: string;
  // a table's mapping draft (§7.8)
  file?: string;
  base?: string;
  rows?: number;
  triples?: number;
  mapping?: Record<string, unknown>;
  preview?: { rows: number; triples: string[] };
  drafted?: 'model' | 'default';
  warnings?: string[];
};

export type IngestTask = {
  id: string;
  dataset: string;
  status: IngestStatus;
  progress: number;
  message?: string;
  createdAt: string;
  updatedAt: string;
  finishedAt?: string;
  input: { name?: string; url?: string; format?: string; bytes?: number; mode?: IngestMode };
  estimate?: IngestEstimate;
  usage?: {
    modelCalls?: number;
    inputTokens?: number;
    outputTokens?: number;
    estimatedCost?: number;
  };
  result?: IngestResult;
  error?: IngestError;
};

export type IngestTaskList = {
  dataset: string;
  tasks: IngestTask[];
  capabilities: { pdf: boolean; ocr: boolean };
};

const WAITING: IngestStatus[] = [
  'awaiting-confirmation',
  'awaiting-approval',
  'done',
  'failed',
  'cancelled',
];

/** Whether a task still runs on the server, and is worth polling. */
export const running = (t: IngestTask) => !WAITING.includes(t.status);

export type IngestOptions = {
  mode?: IngestMode;
  profile?: string;
  title?: string;
  allowPartial?: boolean;
  confirm?: boolean;
  base?: string;
};

/** `POST /$/ingest/{ds}` with a file and the options as fields. */
export function startIngest(ds: string, file: File, opts: IngestOptions = {}) {
  const form = new FormData();
  for (const [k, v] of Object.entries(opts)) {
    if (v !== undefined && v !== '') form.append(k, String(v));
  }
  form.append('file', file, file.name);
  return json<IngestTask>(`/$/ingest/${enc(ds)}`, { method: 'POST', body: form });
}

export const ingestTasks = (ds: string, signal?: AbortSignal) =>
  json<IngestTaskList>(`/$/ingest/${enc(ds)}`, { signal, cache: 'no-store' });

/** `GET /$/ingest/{ds}/{task}`, held up to `wait` seconds while the task runs. */
export const ingestTask = (ds: string, id: string, wait = 0, signal?: AbortSignal) =>
  json<IngestTask>(`/$/ingest/${enc(ds)}/${enc(id)}${wait ? `?wait=${wait}` : ''}`, {
    signal,
    cache: 'no-store',
  });

export const confirmIngest = (ds: string, id: string) =>
  json<IngestTask>(`/$/ingest/${enc(ds)}/${enc(id)}/confirm`, { method: 'POST' });

export const approveIngest = (ds: string, id: string) =>
  json<IngestTask>(`/$/ingest/${enc(ds)}/${enc(id)}/approve`, { method: 'POST' });

export async function cancelIngest(ds: string, id: string): Promise<void> {
  await request(`/$/ingest/${enc(ds)}/${enc(id)}`, { method: 'DELETE' });
}
