// Why is this inferred? (docs/specs/U10-canvas-lenses.md §2.4): the derivation trees of
// `POST /$/reason/{ds}/explain` (docs/API.md#explanations), and what the panel and the
// canvas need from them. No Svelte here.

import * as api from './api';
import type { Term } from './api';
import { termKey } from './rdf';

export type WhyTriple = { s: Term; p: Term; o: Term };

export type Derivation = {
  id: number;
  triple: WhyTriple;
  kind: 'asserted' | 'axiom' | 'derived' | 'ref' | 'unexplained';
  depth: number;
  graph?: string;
  rule?: { name: string; profile: string; owl2rl?: string };
  premises?: Derivation[];
  bindings?: Record<string, Term>;
  builtins?: { call: string; holds: true }[];
  generalized?: boolean;
  missing?: boolean;
  /** A repeat of the node with this id given earlier, without its premises. */
  shared?: boolean;
};

export type Explanation = {
  mode: 'why';
  entailed: true;
  commit: number;
  source: 'materialized' | 'rdfsOnRead' | 'demand';
  partial?: boolean;
  reason?: string;
  trees: Derivation[];
  more: boolean;
  readAtHead?: boolean;
  stale?: boolean | null;
  commitsSince?: number;
  staleReason?: string;
};

/** What the panel shows: the trees, or why there are none. */
export type WhyResult =
  | { ok: true; explanation: Explanation }
  | { ok: false; kind: 'none' | 'budget' | 'unavailable' | 'error'; message: string };

/** The most derivations the panel asks for, whose top rules differ (§2.4). */
export const MAX_TREES = 5;

const NONE = 'No derivation found.';
const BUDGET = 'The derivation is deeper than the search limit.';

/** Ask why `triple` is entailed. A `404`, `422` or `501` is an answer, not an error. */
export async function explainTriple(
  ds: string,
  triple: WhyTriple,
  opts: { maxTrees?: number; maxDepth?: number; signal?: AbortSignal } = {},
): Promise<WhyResult> {
  try {
    const explanation = await api.json<Explanation>(`/$/reason/${encodeURIComponent(ds)}/explain`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        triple,
        mode: 'why',
        maxTrees: opts.maxTrees ?? MAX_TREES,
        ...(opts.maxDepth ? { maxDepth: opts.maxDepth } : {}),
      }),
      signal: opts.signal,
    });
    return { ok: true, explanation };
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    if (e instanceof api.ApiError) {
      if (e.status === 404 && e.code === 'no-visible-derivation')
        return { ok: false, kind: 'none', message: NONE };
      if (e.status === 422 && e.code === 'reasoning-budget')
        return { ok: false, kind: 'budget', message: BUDGET };
      if (e.status === 501 || (e.status === 404 && !e.code) || e.status === 405)
        return {
          ok: false,
          kind: 'unavailable',
          message: 'This server cannot explain inferences.',
        };
    }
    return { ok: false, kind: 'error', message: api.errorMessage(e) };
  }
}

/** The key of a triple, the same as an edge's `triple` gives. */
export function tripleKey(t: WhyTriple): string {
  return `${termKey(t.s)} ${termKey(t.p)} ${termKey(t.o)}`;
}

/** Every node of a tree, the root first, depth first in body order. */
export function walk(d: Derivation, out: Derivation[] = []): Derivation[] {
  out.push(d);
  for (const p of d.premises ?? []) walk(p, out);
  return out;
}

/**
 * The triples **Show on graph** draws: the premises of the tree, each once, in the order
 * the tree lists them. The root itself is the edge the person clicked, already drawn.
 */
export function premiseTriples(d: Derivation): WhyTriple[] {
  const seen = new Set<string>([tripleKey(d.triple)]);
  const out: WhyTriple[] = [];
  for (const n of walk(d).slice(1)) {
    const k = tripleKey(n.triple);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(n.triple);
  }
  return out;
}

/** The asserted leaves of a tree, each once. */
export function leaves(d: Derivation): Derivation[] {
  const seen = new Set<string>();
  return walk(d).filter((n) => {
    if (n.kind !== 'asserted') return false;
    const k = tripleKey(n.triple);
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

/** The rule's name, with its OWL 2 RL id when it differs and its profile, for a node. */
export function ruleText(d: Derivation): string {
  if (!d.rule) return '';
  const id = d.rule.owl2rl && d.rule.owl2rl !== d.rule.name ? ` (OWL 2 RL ${d.rule.owl2rl})` : '';
  return `${d.rule.name}${id}`;
}

/** A short gloss of well-known rules, as the panel shows beside the name. */
export function ruleGloss(name: string): string {
  const g: Record<string, string> = {
    rdfs2: 'domain',
    rdfs3: 'range',
    rdfs5: 'subproperty chain',
    rdfs7: 'subproperty',
    rdfs9: 'subclass',
    rdfs11: 'subclass chain',
    'cax-sco': 'subclass',
    'prp-dom': 'domain',
    'prp-rng': 'range',
    'prp-spo1': 'subproperty',
    'prp-symp': 'symmetric',
    'prp-trp': 'transitive',
    'prp-inv1': 'inverse',
    'prp-inv2': 'inverse',
    'eq-sym': 'sameAs symmetry',
    'eq-trans': 'sameAs transitivity',
    'scm-sco': 'subclass chain',
    'scm-spo': 'subproperty chain',
  };
  return g[name] ?? '';
}

/** How a node came to hold, in a few words: its rule, graph, or that it is cut. */
export function how(d: Derivation, graphLabel: (g: string) => string = (g) => g): string {
  switch (d.kind) {
    case 'asserted':
      return d.graph ? `asserted · graph ${graphLabel(d.graph)}` : 'asserted · RDFS schema';
    case 'axiom':
      return `axiom${d.rule ? ` · ${d.rule.name}` : ''}`;
    case 'unexplained':
      return 'not explained within the search budget';
    case 'ref':
      return 'derived deeper than shown';
    default: {
      const gloss = d.rule ? ruleGloss(d.rule.name) : '';
      return `derived by ${ruleText(d)}${gloss ? ` (${gloss})` : ''}${d.shared ? ' · as above' : ''}`;
    }
  }
}

/** The largest id of the given trees, which ids given out later must pass. */
export function maxId(trees: Derivation[]): number {
  return Math.max(0, ...trees.flatMap((t) => walk(t).map((d) => d.id)));
}

/**
 * Replace the node with id `id` in `tree` by `sub` (a `ref` expanded on request). The
 * expansion's answer numbers its nodes from 1 again, so its nodes, and the shared nodes
 * that point at them, move past `floor`, and its root takes `id`. `floor` is the largest
 * id of every tree of the answer, since the panel keys its state by id across trees.
 */
export function graft(
  tree: Derivation,
  id: number,
  sub: Derivation,
  floor = maxId([tree]),
): Derivation {
  const offset = floor;
  const renumber = (d: Derivation, root: boolean): Derivation => ({
    ...d,
    id: root ? id : d.id + offset,
    premises: d.premises?.map((p) => renumber(p, false)),
  });
  // a root is never among its own premises, so no shared node points at it
  const moved = renumber(sub, true);
  const put = (d: Derivation): Derivation => {
    if (d.id === id && !d.shared) return moved;
    if (!d.premises) return d;
    return { ...d, premises: d.premises.map(put) };
  };
  return put(tree);
}

/**
 * Whether two answers read the same inferences: the same commit and source, and both or
 * neither at the head. An expansion that reads others cannot join the tree it expands.
 */
export function sameRead(a: Explanation, b: Explanation): boolean {
  return a.commit === b.commit && a.source === b.source && !!a.readAtHead === !!b.readAtHead;
}

/** The panel's footer: which derivation of how many, its depth and its commit. */
export function footer(e: Explanation, index: number): string {
  const n = e.trees.length;
  const of = `${index + 1} of ${n}${e.more ? '+' : ''} derivation${n === 1 && !e.more ? '' : 's'}`;
  const t = e.trees[index];
  return `${of} · depth ${t?.depth ?? 0} · from commit ${e.commit}`;
}

/** The note above a stale tree (§2.4), or null for fresh inferences. */
export function staleNote(e: Explanation): string | null {
  if (e.source !== 'materialized') return null;
  // a read at the head describes the current data, whatever the run's freshness
  if (e.readAtHead)
    return 'The commit these inferences were computed at is no longer in the history, so this tree uses the current data.';
  if (e.stale) {
    const since =
      e.commitsSince != null
        ? ` ${e.commitsSince} commit${e.commitsSince === 1 ? '' : 's'} since.`
        : '';
    return `These inferences are out of date. This tree shows them as computed at commit ${e.commit}.${since}`;
  }
  return null;
}

export type WhyNotExplanation = {
  mode: 'whyNot';
  entailed: boolean;
  commit: number;
  rules?: {
    name: string;
    unifier: Record<string, Term>;
    blockedBy?: { call: string; fact?: WhyTriple }[];
  }[];
  hypotheses?: { missing: WhyTriple[]; placeholders: string[]; tree: Derivation }[];
  complete?: boolean;
};

export function explainMissing(
  ds: string,
  triple: WhyTriple,
  signal?: AbortSignal,
): Promise<WhyNotExplanation> {
  return api.json<WhyNotExplanation>(`/$/reason/${encodeURIComponent(ds)}/explain`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ triple, mode: 'whyNot', maxMissing: 2, maxHypotheses: 10 }),
    signal,
  });
}
