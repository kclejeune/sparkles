// A hierarchy read one level at a time from `GET /$/ontology/{ds}/hierarchy` (ROADMAP item
// 607): the ontology editor's class and property trees. A level is read when its node opens,
// the path to a selected entity is read from the route's ancestors, and a filter reads the
// entity search's hits with their ancestors as a tree of its own.

import { buildTree, type Tree } from './ontology';
import type { Hierarchy, TreeNode } from './browse';

/** The children of one level that the tree reads at most. */
export const LEVEL_LIMIT = 1000;

export type LevelReader = (q: { iri?: string; ancestors?: boolean }) => Promise<Hierarchy>;

export class LevelTree {
  /** The children of each read level, by parent, with `''` for the roots. */
  levels = $state<Record<string, string[]>>({});
  /** The number of children of each node the tree has seen. */
  counts = $state<Record<string, number>>({});
  /** The levels that hold fewer children than the node has, past `LEVEL_LIMIT`. */
  clipped = $state<Record<string, number>>({});
  /** Whether the roots are being read. */
  loading = $state(false);
  error = $state<string | null>(null);
  #pending = new Map<string, Promise<void>>();
  #generation = 0;

  constructor(
    private read: LevelReader,
    /** Called with every node an answer names, for the page's labels. */
    private onnames: (nodes: TreeNode[]) => void = () => {},
  ) {}

  roots(): string[] {
    return this.levels[''] ?? [];
  }

  children(id: string): string[] {
    return this.levels[id] ?? [];
  }

  /** Whether `id` has children that the tree has not read. */
  more(id: string): boolean {
    return (this.counts[id] ?? 0) > 0 && !(id in this.levels);
  }

  /** Forget every level, for a new commit or graph, and read the roots and `open` again. */
  reset(open: Iterable<string> = []) {
    this.#generation++;
    this.#pending.clear();
    this.levels = {};
    this.counts = {};
    this.clipped = {};
    void this.load(null);
    for (const id of open) void this.load(id);
  }

  #take(nodes: TreeNode[]) {
    if (!nodes.length) return;
    const counts = { ...this.counts };
    for (const n of nodes) counts[n.iri] = n.children;
    this.counts = counts;
    this.onnames(nodes);
  }

  /** Read the children of `id`, or the roots for null, once. */
  load(id: string | null): Promise<void> {
    const key = id ?? '';
    if (key in this.levels) return Promise.resolve();
    const was = this.#pending.get(key);
    if (was) return was;
    const gen = this.#generation;
    if (!id) this.loading = true;
    const p = this.read(id ? { iri: id } : {})
      .then((h) => {
        if (gen !== this.#generation) return;
        this.#take([...(h.node ? [h.node] : []), ...h.children]);
        this.levels = { ...this.levels, [key]: h.children.map((c) => c.iri) };
        if (h.total > h.children.length) this.clipped = { ...this.clipped, [key]: h.total };
        if (!id) this.error = null;
      })
      .catch((e: unknown) => {
        if (gen === this.#generation && !id) this.error = (e as Error).message ?? String(e);
      })
      .finally(() => {
        this.#pending.delete(key);
        if (!id && gen === this.#generation) this.loading = false;
      });
    this.#pending.set(key, p);
    return p;
  }

  /**
   * Read the path from a root to `iri`: its ancestors from the route, and the level of each
   * so the path shows in place. Answers the ancestors to open, nearest last.
   */
  async reveal(iri: string): Promise<string[]> {
    const gen = this.#generation;
    const h = await this.read({ iri, ancestors: true });
    if (gen !== this.#generation) return [];
    const anc = h.ancestors;
    if (!anc) return [];
    this.#take(anc.nodes);
    const ids = new Set<string>();
    const parents = new Map<string, string[]>();
    for (const [c, p] of anc.edges) parents.set(c, [...(parents.get(c) ?? []), p]);
    const walk = (n: string) => {
      for (const p of parents.get(n) ?? []) if (!ids.has(p)) (ids.add(p), walk(p));
    };
    walk(iri);
    await Promise.all([null, ...ids].map((a) => this.load(a)));
    return [...ids];
  }
}

/**
 * The tree a filter shows: `hits` and their ancestors from the route, joined by the
 * route's child–parent edges. Each hit's reader answer carries its ancestors.
 */
export function filteredTree(answers: Hierarchy[]): Tree {
  const nodes = new Set<string>();
  const edges: [string, string][] = [];
  for (const h of answers) {
    if (h.node) nodes.add(h.node.iri);
    for (const n of h.ancestors?.nodes ?? []) nodes.add(n.iri);
    for (const e of h.ancestors?.edges ?? []) edges.push(e);
  }
  return buildTree(nodes, edges);
}

/**
 * Scrolls the row of `iri` in a tree into view once the tree has drawn it, so a selection
 * that arrives by link shows in its tree as well as in the detail panel.
 */
export function scrollToRow(iri: string, tries = 10) {
  if (typeof document === 'undefined') return;
  const row = document.querySelector<HTMLElement>(
    `[role="tree"] li[data-id="${CSS.escape(iri)}"] > .node`,
  );
  if (row) row.scrollIntoView({ block: 'nearest' });
  else if (tries > 0) requestAnimationFrame(() => scrollToRow(iri, tries - 1));
}
