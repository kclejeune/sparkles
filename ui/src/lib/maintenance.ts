// Memory maintenance (C18 §8.3 and §8.4): consolidation and retention of agent memory.
// Both run as tasks of the ingestion registry, so `GET /$/ingest/{ds}/{task}` follows
// them. `GET /$/memory/{ds}/maintenance` says when a scheduled pass last started and when
// the next one is due. The helpers below say a pass's outcome in words and list what a
// retention pass would delete.

import { json } from './api';
import { fmtInt, fmtRelative } from './format';
import type { IngestTask } from './review-api';

const enc = encodeURIComponent;

export type MaintenanceJob = 'consolidation' | 'retention';

/** One job of `GET /$/memory/{ds}/maintenance`. */
export type MaintenanceEntry = {
  settings: Record<string, unknown> | null;
  /** When a scheduled pass last started. */
  lastRun?: string;
  /** Its task. */
  lastTask?: string;
  /** A time, or `due` when the next look (within a minute) starts it. */
  nextRun?: string;
};

export type MaintenanceStatus = {
  dataset: string;
  consolidation: MaintenanceEntry;
  retention: MaintenanceEntry;
};

export const maintenanceStatus = (ds: string, signal?: AbortSignal) =>
  json<MaintenanceStatus>(`/$/memory/${enc(ds)}/maintenance`, { signal, cache: 'no-store' });

const post = (body: unknown): RequestInit => ({
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
});

/** Start a consolidation pass with the dataset's settings. */
export const startConsolidation = (ds: string, opts: { dryRun?: boolean } = {}) =>
  json<IngestTask>(`/$/memory/${enc(ds)}/consolidate`, post(opts));

/** Start a retention pass with the dataset's settings; a dry run deletes nothing. */
export const startRetention = (ds: string, opts: { dryRun?: boolean } = {}) =>
  json<IngestTask>(`/$/memory/${enc(ds)}/retention`, post(opts));

/** The kind of a maintenance task, or null for any other ingestion task. */
export function jobOf(t: Pick<IngestTask, 'input'>): MaintenanceJob | null {
  const k = (t.input as { kind?: unknown } | undefined)?.kind;
  return k === 'consolidation' || k === 'retention' ? k : null;
}

/** Whether a maintenance task was a dry run, which changes nothing. */
export const isDryRun = (t: Pick<IngestTask, 'input'>) =>
  (t.input as { dryRun?: unknown } | undefined)?.dryRun === true;

/** The newest task of a job in a task list, dry runs left out. */
export function latestOf<T extends Pick<IngestTask, 'input' | 'createdAt'>>(
  tasks: readonly T[],
  job: MaintenanceJob,
): T | null {
  let best: T | null = null;
  for (const t of tasks)
    if (jobOf(t) === job && !isDryRun(t) && (!best || t.createdAt > best.createdAt)) best = t;
  return best;
}

const plural = (n: number, one: string, many = `${one}s`) => `${fmtInt(n)} ${n === 1 ? one : many}`;
const num = (v: unknown) => (typeof v === 'number' ? v : Array.isArray(v) ? v.length : 0);
/** A count of the result, or `fallback` when the result leaves it out (a task list's summary). */
const count = (v: unknown, one: string, fallback: string) =>
  typeof v === 'number' || Array.isArray(v) ? plural(num(v), one) : fallback;

/** A finished pass in a sentence: its outcome and what it did. */
export function outcomeText(
  t: Pick<IngestTask, 'status' | 'result' | 'error' | 'message'>,
): string {
  if (t.status === 'failed') return `Failed: ${t.error?.message ?? t.message ?? 'no reason given'}`;
  if (t.status === 'cancelled') return 'Cancelled';
  if (t.status !== 'done') return t.message ?? 'Running';
  const r = (t.result ?? {}) as Record<string, unknown>;
  const branch = typeof r.branch === 'string' ? r.branch : null;
  switch (r.outcome) {
    case 'proposed':
      return `Proposed ${count(r.proposed, 'fact', 'facts')}${branch ? ` on ${branch}` : ''} for review`;
    case 'merged':
      return `Merged ${count(r.proposed, 'fact', 'the facts')} into main`;
    case 'dry-run':
      return 'delete' in r
        ? `Dry run: ${count(r.delete, 'session graph', 'some session graphs')} would be deleted`
        : `Dry run: ${count(r.repeated, 'repeated fact', 'repeated facts')} found`;
    case 'nothing-to-consolidate':
      return typeof r.scanned === 'number'
        ? `Nothing to consolidate in ${plural(r.scanned, 'fact')}`
        : 'Nothing to consolidate';
    case 'no-facts':
      return 'No fact passed its checks, so nothing was proposed';
    case 'pending-review':
      return `An earlier consolidation${branch ? ` on ${branch}` : ''} waits for review`;
    case 'deleted': {
      const errors = num(r.errors);
      return `Deleted ${count(r.deleted, 'session graph', 'old session graphs')}${errors ? `, ${plural(errors, 'failure')}` : ''}`;
    }
    case 'nothing-to-delete':
      return 'No session graph is old enough to delete';
    default:
      return typeof r.outcome === 'string' ? r.outcome : 'Done';
  }
}

/** When the next scheduled pass runs, in words; null without a schedule. */
export function nextRunText(next: string | undefined, now = Date.now()): string | null {
  if (!next) return null;
  if (next === 'due') return 'due within a minute';
  const at = Date.parse(next);
  if (Number.isNaN(at)) return next;
  if (at <= now) return 'due within a minute';
  const s = Math.round((at - now) / 1000);
  if (s < 3600) return `in ${Math.max(1, Math.round(s / 60))}m`;
  if (s < 86400) return `in ${Math.round(s / 3600)}h`;
  return `in ${Math.round(s / 86400)}d`;
}

/** When a pass started, in words. */
export const startedText = (iso: string | undefined, now = Date.now()) => fmtRelative(iso, now);

/** A graph of a retention scan. */
export type RetentionGraph = {
  graph: string;
  newest?: string | null;
  ageDays?: number;
  delete?: boolean;
  kept?: 'recent' | 'no-time' | 'unconsolidated' | 'too-many-facts';
};

/** What a retention dry run found: the graphs it would delete and those it keeps. */
export function retentionPlan(result: unknown): {
  after: string;
  remove: RetentionGraph[];
  keep: RetentionGraph[];
} {
  const r = (result ?? {}) as { after?: unknown; graphs?: unknown; delete?: unknown };
  const graphs = (Array.isArray(r.graphs) ? r.graphs : []) as RetentionGraph[];
  const del = new Set(Array.isArray(r.delete) ? r.delete.map(String) : []);
  return {
    after: typeof r.after === 'string' ? r.after : '',
    remove: graphs.filter((g) => g.delete || del.has(g.graph)),
    keep: graphs.filter((g) => !(g.delete || del.has(g.graph))),
  };
}

/** Why a retention pass keeps a graph, in words. */
export const KEPT_TEXT: Record<NonNullable<RetentionGraph['kept']>, string> = {
  recent: 'too recent',
  'no-time': 'no time on its facts',
  unconsolidated: 'has facts no reviewed graph asserts',
  'too-many-facts': 'more than 5,000 facts',
};
