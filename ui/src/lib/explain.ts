// Explaining a query (C18 §6.6): `POST /{ds}/sparql/explain` over server-sent events,
// the streamed plan read as an eager one, and the marks the notes put on the plan tree.

import { request, type CursorPlan, type PlanNode } from './api';
export { isCursorPlan, planOf } from './api';
import { SseParser } from './ask-api';

const enc = encodeURIComponent;

export type Severity = 'high' | 'warning' | 'info';

/** A note on an operator, or on the whole query when `node` is null. */
export type ExplainNote = {
  node: string | null;
  code: string;
  severity: Severity;
  text: string;
  source: 'explain' | 'planner' | 'lint' | 'schema' | 'model';
  /** The editor range of a lint finding that names no node. */
  range?: { line: number; column: number; endLine: number; endColumn: number };
};

/** A sentence of **What it asks**, with the nodes it describes. */
export type ExplainSentence = { text: string; nodes: string[] };

/** The facts of one operator. */
export type NodeFacts = {
  id: string;
  operator: string;
  description: string;
  estimatedRows: number | null;
  actualRows: number | null;
  timeMs: number;
  selfMs: number;
  complete: boolean;
  partial?: boolean;
  skipped?: string;
  stoppedEarly?: boolean;
};

export type Explanation = {
  source: 'template' | 'model';
  asks: ExplainSentence[];
  notes: ExplainNote[];
  provider?: string;
  model?: string;
  dropped?: number;
  replaced?: number;
  /** The template text, one click away from model text. */
  template?: { asks: ExplainSentence[] };
  fallback?: unknown;
};

export type ExplainRequest = {
  query: string;
  profile?: 'estimate' | 'run' | 'given';
  plan?: PlanNode | CursorPlan;
  commit?: number;
  /** The error body of a run that a budget stopped. */
  error?: Record<string, unknown>;
  describe?: boolean;
  at?: string | number;
  branch?: string;
  reasoning?: boolean;
};

export type ExplainPlanEvent = {
  queryType: string;
  executed: boolean;
  plan: PlanNode | CursorPlan;
  stop?: { budget: string; limit?: number; elapsedMs?: number };
};

export type ExplainNotesEvent = {
  nodes: NodeFacts[];
  notes: ExplainNote[];
  shownNotes: number;
  hiddenEstimates: boolean;
};

export type ExplainEvent =
  | { event: 'plan'; data: ExplainPlanEvent }
  | { event: 'notes'; data: ExplainNotesEvent }
  | { event: 'explanation'; data: Explanation }
  | { event: 'usage'; data: Record<string, unknown> }
  | { event: 'error'; data: { error: string; code: string } };

/**
 * `POST /{ds}/sparql/explain` over server-sent events. `on` gets each event as it
 * arrives, so the notes show before a model answers. A refusal before the stream starts
 * rejects with the `ApiError`.
 */
export async function explainStream(
  ds: string,
  body: ExplainRequest,
  on: (e: ExplainEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  const res = await request(`/${enc(ds)}/sparql/explain`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
    body: JSON.stringify(body),
    signal,
  });
  const parser = new SseParser();
  const deliver = (text: string) => {
    for (const { event, data } of parser.push(text)) {
      try {
        on({ event, data: JSON.parse(data) } as ExplainEvent);
      } catch {
        /* not JSON: skipped */
      }
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

const RANK: Record<Severity, number> = { high: 0, warning: 1, info: 2 };

/** The most severe note of each node, which marks it in the tree. */
export function marks(notes: ExplainNote[]): Map<string, Severity> {
  const out = new Map<string, Severity>();
  for (const n of notes) {
    if (!n.node) continue;
    const had = out.get(n.node);
    if (!had || RANK[n.severity] < RANK[had]) out.set(n.node, n.severity);
  }
  return out;
}

/** Whether a sentence or note cites node `id`. */
export function cites(s: ExplainSentence | ExplainNote, id: string): boolean {
  return 'nodes' in s ? s.nodes.includes(id) : s.node === id;
}

/** The ids of the ancestors of `id`, root first (`0.1.2` → `0`, `0.1`). */
export function ancestors(id: string): string[] {
  const parts = id.split('.');
  return parts.slice(1).map((_, i) => parts.slice(0, i + 1).join('.'));
}
