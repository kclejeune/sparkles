// The HTTP calls of C18: checking a query, diagnosing an empty result, recalling memory,
// the memory settings, the suggested examples of stored queries, and from Phase 2 asking
// the server a question, the assistant settings and the history of asked questions.

import { json, normalizeResult, request, type SparklesResult } from './api';

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
  /** Where the fault most likely lies (Phase 2). */
  verdict?: 'query' | 'data' | 'unknown';
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

// --- asking the server (Phase 2) ------------------------------------------------------

/** A provider and model. */
export type Pair = { provider: string; model: string };

/** What `GET /$/assistant/{ds}` says about asking. */
export type AssistantStatus = {
  models: boolean;
  historyDays: number;
  ask: boolean;
  reason?: string;
  draft?: Pair[];
  /** Whether an answer can carry a summary. */
  summary?: boolean;
};

/** A dataset's `assistant.json` with the server's `status`. */
export type AssistantSettings = {
  enabled?: boolean;
  ask?: boolean;
  send?: 'schema' | 'rows' | 'documents';
  historyDays?: number;
  status?: AssistantStatus;
  [k: string]: unknown;
};

export const assistantSettings = (ds: string, signal?: AbortSignal) =>
  json<AssistantSettings>(`/$/assistant/${enc(ds)}`, { signal, cache: 'no-store' });

/** The body of `POST /{ds}/ask`. */
export type AskRequest = {
  question: string;
  context?: { question: string; query: string }[];
  clarification?: { id: string; value: string };
  at?: string;
  branch?: string;
  reasoning?: boolean;
  run?: boolean;
  summary?: boolean;
  maxRows?: number;
  tryHarder?: string;
  reviewedOnly?: boolean;
  query?: string;
};

/** The draft's graph variables for the graph view, with or without `?`. */
export type AskGraph = { subject: string; predicate?: string; object: string };

export type AskDraft = {
  attempt: number;
  role: string;
  provider: string;
  model: string;
  query: string;
  explanation?: string;
  assumptions?: string[];
  graph?: AskGraph | null;
};

export type AskClarify = {
  id: string;
  question: string;
  choices: { label: string; value: string }[];
};

/** The checked query, and from a run its rows in the form `/{ds}/sparql` gives the UI. */
export type AskResult = {
  query?: string;
  explanation?: string;
  assumptions?: string[];
  terms?: CheckTerm[];
  issues?: CheckIssue[];
  graph?: AskGraph | null;
  commit?: number;
  attempt?: number;
  verdict?: 'query' | 'data' | 'unknown';
  limitAdded?: boolean;
  results?: SparklesResult;
};

export type AskSummary = {
  text: string;
  citations: number[];
  rowsSent?: number;
  provider?: string;
  model?: string;
  uncited?: boolean;
};

export type AskStep = {
  role: string;
  provider: string;
  model: string;
  outcome: string;
  latencyMs?: number;
  inputTokens?: number;
  outputTokens?: number;
  signal?: string;
};

export type AskEscalation = { role: string; from: Pair; to: Pair; signal: string };

export type AskUsage = {
  outcome?: string;
  askId?: string;
  inputTokens?: number;
  outputTokens?: number;
  estimatedCost?: number;
  steps?: AskStep[];
  escalations?: AskEscalation[];
  answeredBy?: Pair & { role: string };
  tryHarder?: boolean;
  smallModel?: boolean;
  level?: string;
  notes?: string[];
};

export type AskError = { code: string; message: string; result?: AskResult; resetAt?: string };

/** One server-sent event of an ask. */
export type AskEvent =
  | { event: 'ground'; data: Record<string, unknown> }
  | { event: 'clarify'; data: AskClarify }
  | { event: 'draft'; data: AskDraft }
  | { event: 'escalate'; data: AskEscalation }
  | { event: 'check'; data: CheckResult }
  | { event: 'run'; data: { attempt: number; rows?: number; error?: AskError } }
  | {
      event: 'diagnosis';
      data: { attempt?: number; kind?: string; text?: string } & Partial<Diagnosis>;
    }
  | { event: 'result'; data: AskResult }
  | { event: 'summary'; data: AskSummary }
  | { event: 'usage'; data: AskUsage }
  | { event: 'error'; data: AskError };

/**
 * Splits a server-sent event stream into events as its text arrives. Comments and
 * keep-alives are skipped, and an event's `data` lines are joined with newlines.
 */
export class SseParser {
  private buf = '';
  push(text: string): { event: string; data: string }[] {
    this.buf += text.replace(/\r\n?/g, '\n');
    const out: { event: string; data: string }[] = [];
    let end: number;
    while ((end = this.buf.indexOf('\n\n')) >= 0) {
      const block = this.buf.slice(0, end);
      this.buf = this.buf.slice(end + 2);
      let event = 'message';
      const data: string[] = [];
      for (const line of block.split('\n')) {
        if (!line || line.startsWith(':')) continue;
        const i = line.indexOf(':');
        const field = i < 0 ? line : line.slice(0, i);
        const value = i < 0 ? '' : line.slice(i + 1).replace(/^ /, '');
        if (field === 'event') event = value;
        else if (field === 'data') data.push(value);
      }
      if (data.length) out.push({ event, data: data.join('\n') });
    }
    return out;
  }
}

/**
 * `POST /{ds}/ask` over server-sent events. It calls `on` for each event as it arrives
 * and resolves when the stream ends. A refusal before the stream starts, such as
 * `no-assistant` or `budget-exceeded`, rejects with the `ApiError`.
 */
export async function askStream(
  ds: string,
  body: AskRequest,
  on: (e: AskEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  const res = await request(`/${enc(ds)}/ask`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
    body: JSON.stringify(body),
    signal,
  });
  const parser = new SseParser();
  const deliver = (text: string) => {
    for (const { event, data } of parser.push(text)) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(data);
      } catch {
        continue;
      }
      if (event === 'result') {
        const r = parsed as AskResult;
        if (r.results) r.results = normalizeResult(r.results);
      }
      on({ event, data: parsed } as AskEvent);
    }
  };
  const reader = res.body?.getReader();
  if (!reader) {
    deliver((await res.text()) + '\n\n');
    return;
  }
  const dec = new TextDecoder();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    deliver(dec.decode(value, { stream: true }));
  }
  deliver(dec.decode() + '\n\n');
}

/** One entry of the caller's server-side history. */
export type AskRecord = {
  id: string;
  at: string;
  question: string;
  query?: string;
  commit?: number;
  result: string;
  outcome: 'none' | 'accepted' | 'edited' | 'rejected';
  note?: string;
};

export async function askHistory(ds: string, signal?: AbortSignal): Promise<AskRecord[]> {
  const body = await json<{ asks: AskRecord[] }>(`/$/asks/${enc(ds)}?limit=50`, {
    signal,
    cache: 'no-store',
  });
  return body?.asks ?? [];
}

export async function deleteAsk(ds: string, id?: string): Promise<void> {
  await request(`/$/asks/${enc(ds)}${id ? `?id=${enc(id)}` : ''}`, { method: 'DELETE' });
}

export type Feedback = 'accepted' | 'edited' | 'rejected';

export async function askFeedback(ds: string, id: string, outcome: Feedback): Promise<void> {
  await request(`/$/asks/${enc(ds)}/${enc(id)}/feedback`, post({ outcome }));
}
