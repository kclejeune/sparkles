// GraphRAG community reports (spec C22, docs/API.md "GraphRAG"): the client of
// `/$/graphrag/{ds}`, and the helpers the Communities tab uses to show the hierarchy, a
// build's estimate and its outcome.

import { getTask, json, type Task } from './api';

const enc = encodeURIComponent;

export type ReportStatus = 'unreviewed' | 'reviewed' | 'rejected' | 'stale' | 'failed';
export const STATUSES: readonly ReportStatus[] = [
  'unreviewed',
  'reviewed',
  'rejected',
  'stale',
  'failed',
];

/** `GET /$/graphrag/{ds}`. */
export type GraphragStatus = {
  dataset: string;
  enabled: boolean;
  reportsGraph: string;
  refresh?: 'manual' | 'auto';
  /** Whether the caller may read the reports graph. */
  available: boolean;
  reason?: string;
  reports?: Partial<Record<ReportStatus, number>>;
  levels?: { level: number; communities: number }[];
  lastBuild?: {
    sourceCommit: number;
    branch: string;
    finishedAt: string;
    tokens: number;
    pending: number;
  };
  /** For a dataset admin only. */
  budget?: { tokensPerBuild: number; tokensPerDay: number; usedToday: number };
  anomalies?: { provider?: string; reported?: number; reserved?: number }[];
};

export type ReportRef = {
  iri: string;
  title: string | null;
  status: ReportStatus | null;
  rating: number | null;
};

export type CommunityRow = {
  iri: string;
  level: number;
  parent: string | null;
  memberCount: number;
  report: ReportRef | null;
};

export type Communities = {
  available: boolean;
  reportsGraph?: string;
  communities: CommunityRow[];
};

export type Finding = {
  text: string;
  about: string[];
  cites: string[];
  unmatchedNames: string[];
};

export type Report = {
  iri: string;
  title: string | null;
  summary: string | null;
  status: ReportStatus;
  rating: number | null;
  failure: string | null;
  revisionOf: string | null;
  droppedCitations: number | null;
  droppedUnsupported: number | null;
  sourceCommit: number | null;
  activity?: { model: string; inputTokens: number; outputTokens: number; startedAt: string };
  findings: Finding[];
};

export type Community = {
  iri: string;
  level: number;
  parent: string | null;
  memberCount: number;
  factCount: number;
  members: { iri: string; label: string | null }[];
  children: { iri: string; title: string | null }[];
  report?: Report | null;
};

/** The estimate a waiting build stores in its task's `detail`. */
export type Estimate = {
  communities: number;
  requests: number;
  leafRequests?: number;
  model: string;
  tokens: number;
  tokensPerBuild: number;
  tokensPerDay: number;
  source?: string;
};

/** The outcome a finished build stores in its task's `detail`. */
export type Outcome = {
  mode: 'build' | 'refresh';
  communities: number;
  written: number;
  kept: number;
  failed: number;
  pending: number;
  tokens: number;
  stopped?: string | null;
};

const post = (path: string, body: unknown) =>
  json<unknown>(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });

export const status = (ds: string, signal?: AbortSignal) =>
  json<GraphragStatus>(`/$/graphrag/${enc(ds)}`, { signal, cache: 'no-store' });

export const communities = (ds: string, signal?: AbortSignal) =>
  json<Communities>(`/$/graphrag/${enc(ds)}/communities`, { signal, cache: 'no-store' });

export const community = (ds: string, iri: string, signal?: AbortSignal) =>
  json<Community>(`/$/graphrag/${enc(ds)}/community?iri=${enc(iri)}`, {
    signal,
    cache: 'no-store',
  });

/** Start a build or a refresh. Without `confirm` the task waits for {@link confirm}. */
export const start = (ds: string, mode: 'build' | 'refresh', confirm = false) =>
  post(`/$/graphrag/${enc(ds)}/${mode}`, { confirm }) as Promise<Task>;

export const confirm = (ds: string, task: string, yes: boolean) =>
  post(`/$/graphrag/${enc(ds)}/confirm`, { task, confirm: yes });

export const review = (
  ds: string,
  report: string,
  change: { status?: 'reviewed' | 'rejected'; title?: string; summary?: string },
) =>
  post(`/$/graphrag/${enc(ds)}/review`, { report, ...change }) as Promise<{
    report: string;
    status: ReportStatus;
    commit: number | null;
  }>;

export const task = (id: string) => getTask(id);

export const ended = (t: Task) =>
  t.state === 'done' || t.state === 'failed' || t.state === 'cancelled';

/** The estimate of a task that waits for its confirmation, if it does. */
export function waitingEstimate(t: Task | null): Estimate | null {
  const d = t?.detail as { phase?: string; estimate?: Estimate } | undefined;
  return t && !ended(t) && d?.phase === 'awaiting-confirmation' && d.estimate ? d.estimate : null;
}

/** The outcome of a finished task, if it has one. */
export function outcomeOf(t: Task | null): Outcome | null {
  const d = t?.detail as Partial<Outcome> | undefined;
  return t?.state === 'done' && d && typeof d.written === 'number' ? (d as Outcome) : null;
}

/** A line about a finished run. */
export function outcomeText(o: Outcome): string {
  const parts = [`${o.written} report${o.written === 1 ? '' : 's'} written`, `${o.kept} kept`];
  if (o.failed) parts.push(`${o.failed} failed`);
  if (o.pending) parts.push(`${o.pending} pending`);
  let t = `${parts.join(', ')}, ${o.tokens.toLocaleString('en')} tokens.`;
  if (o.stopped) t += ` Stopped: ${o.stopped}`;
  return t;
}

/** A community of the tree with its children, coarsest level first. */
export type TreeNode = CommunityRow & { children: TreeNode[] };

/**
 * The communities as a forest: each community under its parent, the roots being those
 * without a parent in the list, children ordered by rating and then by IRI.
 */
export function tree(rows: CommunityRow[]): TreeNode[] {
  const nodes = new Map<string, TreeNode>();
  for (const r of rows) nodes.set(r.iri, { ...r, children: [] });
  const roots: TreeNode[] = [];
  for (const n of nodes.values()) {
    const p = n.parent ? nodes.get(n.parent) : undefined;
    if (p) p.children.push(n);
    else roots.push(n);
  }
  const order = (a: TreeNode, b: TreeNode) =>
    (b.report?.rating ?? -1) - (a.report?.rating ?? -1) || a.iri.localeCompare(b.iri);
  const sort = (l: TreeNode[]) => {
    l.sort(order);
    for (const n of l) sort(n.children);
  };
  sort(roots);
  return roots;
}

/** A cited fact as the server gives it, an N-Triples triple term, shown shorter. */
export function citeText(c: string): string {
  const m = /^<<\(\s*(.*?)\s*\)>>$/.exec(c.trim());
  return m ? m[1] : c;
}
