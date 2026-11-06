// Named individuals grouped by their asserted named classes. Pages are pinned to the
// first root response's revision. Synthetic group keys cannot collide with entity IRIs.
import type { Name } from './browse';

export const INDIVIDUAL_LIMIT = 100;
export const groupKey = (iri: string | null) => (iri == null ? '@untyped' : `@type:${iri}`);
export const groupIri = (key: string) => (key.startsWith('@type:') ? key.slice(6) : null);
export const isGroup = (key: string) => key === '@untyped' || key.startsWith('@type:');
export type IndividualPage = {
  inferenceSource?: import('./inferred-read').MaterializationSource;
  typeBasis?: 'materializedNamedTypes';
  commit: number;
  groups: { type: Name | null; total: number | null }[];
  individuals: (Name & { types: Name[] })[];
  total: number;
  next: string | null;
};
export type IndividualQuery = {
  type?: string;
  untyped?: boolean;
  iri?: string;
  after?: string;
  at?: string;
  limit?: number;
};
export type IndividualReader = (q: IndividualQuery, signal: AbortSignal) => Promise<IndividualPage>;
type ReadControl = { signal?: AbortSignal; read?: IndividualReader };

export class IndividualTree {
  levels = $state<Record<string, string[]>>({});
  counts = $state<Record<string, number>>({});
  types = $state<Record<string, string[]>>({});
  loading = $state(false);
  error = $state<string | null>(null);
  #next = new Map<string, string | null>();
  #pending = new Map<string, { promise: Promise<void>; signal: AbortSignal }>();
  #ctl = new AbortController();
  #generation = 0;
  #commit: number | null = null;

  constructor(
    private read: IndividualReader,
    private onnames: (nodes: Name[]) => void = () => {},
  ) {}

  get commit() {
    return this.#commit;
  }

  roots() {
    return this.levels[''] ?? [];
  }
  children(id: string) {
    return this.levels[id] ?? [];
  }
  more(id: string) {
    return isGroup(id) && !(id in this.levels);
  }
  clippedOf(id: string | null) {
    const key = id ?? '';
    return this.#next.get(key) != null
      ? { shown: this.levels[key]?.length ?? 0, total: this.counts[key] ?? 0 }
      : null;
  }

  drop() {
    this.#generation++;
    this.#ctl.abort();
    this.#ctl = new AbortController();
    this.#commit = null;
    this.#pending.clear();
    this.#next.clear();
    this.levels = {};
    this.counts = {};
    this.types = {};
    this.loading = false;
    this.error = null;
  }

  reset(open: Iterable<string> = []) {
    this.drop();
    const gen = this.#generation;
    void this.load(null).then(() => {
      if (gen === this.#generation)
        for (const id of open) if (isGroup(id) && this.roots().includes(id)) void this.load(id);
    });
  }

  #take(page: IndividualPage) {
    const names = [
      ...page.groups.flatMap((g) => (g.type ? [g.type] : [])),
      ...page.individuals.flatMap((i) => [i, ...i.types]),
    ];
    this.onnames(names);
    const types = { ...this.types };
    for (const i of page.individuals) types[i.iri] = i.types.map((t) => t.iri);
    this.types = types;
  }

  load(id: string | null, control: ReadControl = {}): Promise<void> {
    if (control.signal?.aborted) return Promise.resolve();
    const key = id ?? '';
    if (key in this.levels || (id != null && !isGroup(id))) return Promise.resolve();
    return this.#load(id, false, control);
  }

  loadMore(id: string | null, control: ReadControl = {}): Promise<void> {
    if (control.signal?.aborted) return Promise.resolve();
    if (this.#next.get(id ?? '') == null) return Promise.resolve();
    return this.#load(id, true, control);
  }

  #load(id: string | null, append: boolean, control: ReadControl): Promise<void> {
    const key = id ?? '';
    const pending = this.#pending.get(key);
    if (pending && !pending.signal.aborted) return pending.promise;
    const gen = this.#generation;
    const signal = control.signal
      ? AbortSignal.any([this.#ctl.signal, control.signal])
      : this.#ctl.signal;
    if (id == null) this.loading = true;
    const p = (async () => {
      // A child never races the root's revision anchor.
      if (id != null && this.#commit == null) await this.load(null);
      if (gen !== this.#generation || signal.aborted) return;
      const page = await (control.read ?? this.read)(
        {
          ...(id ? (id === '@untyped' ? { untyped: true } : { type: groupIri(id)! }) : {}),
          ...(append ? { after: this.#next.get(key)! } : {}),
          ...(this.#commit != null ? { at: `commit:${this.#commit}` } : {}),
          limit: INDIVIDUAL_LIMIT,
        },
        signal,
      );
      if (gen !== this.#generation || signal.aborted) return;
      if (this.#commit != null && page.commit !== this.#commit)
        throw new Error('The individuals page changed revision. Reopen the tab.');
      this.#commit = page.commit;
      this.#take(page);
      const ids =
        id == null
          ? page.groups.map((g) => groupKey(g.type?.iri ?? null))
          : page.individuals.map((i) => i.iri);
      // A locate may have inserted rows outside the normal continuation. Keep them,
      // but only the response cursor determines the next regular page.
      this.levels = { ...this.levels, [key]: [...new Set([...(this.levels[key] ?? []), ...ids])] };
      const counts = { ...this.counts, [key]: page.total };
      for (const g of page.groups)
        if (g.total != null) counts[groupKey(g.type?.iri ?? null)] = g.total;
      this.counts = counts;
      this.#next.set(key, page.next);
      this.error = null;
    })()
      .catch((e: unknown) => {
        if (gen === this.#generation && !signal.aborted)
          this.error = (e as Error).message ?? String(e);
      })
      .finally(() => {
        if (gen !== this.#generation) return;
        if (this.#pending.get(key)?.promise === p) {
          this.#pending.delete(key);
          if (id == null) this.loading = false;
        }
      });
    this.#pending.set(key, { promise: p, signal });
    return p;
  }

  async reveal(iri: string, control: ReadControl = {}): Promise<string[]> {
    const gen = this.#generation;
    const signal = control.signal
      ? AbortSignal.any([this.#ctl.signal, control.signal])
      : this.#ctl.signal;
    await this.load(null);
    if (gen !== this.#generation || signal.aborted || this.#commit == null) return [];
    try {
      const page = await (control.read ?? this.read)(
        { iri, at: `commit:${this.#commit}`, limit: 1 },
        signal,
      );
      if (gen !== this.#generation || signal.aborted || page.commit !== this.#commit) return [];
      this.#take(page);
      const groups = page.groups.map((g) => groupKey(g.type?.iri ?? null));
      // Read just the first page of each selected membership. No earlier member pages
      // are required even when this individual sorts after millions of other rows.
      await Promise.all(groups.map((g) => this.load(g, control)));
      if (gen !== this.#generation || signal.aborted) return [];
      const levels: Record<string, string[]> = {
        ...this.levels,
        '': [...new Set([...this.roots(), ...groups])],
      };
      for (const g of groups) levels[g] = [...new Set([...(levels[g] ?? []), iri])];
      this.levels = levels;
      return groups;
    } catch (e) {
      if (gen === this.#generation && !signal.aborted) this.error = (e as Error).message;
      return [];
    }
  }
}
