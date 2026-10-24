// The selection the views of one dataset share (docs/specs/U11-linked-views.md, section
// 2.1): a set of entity terms, named by the source that made it. The graph canvas and the
// maps read and write it, so that selecting in one highlights the same entities in the
// others. Entities are keyed by `termKey` (`<iri>`), which the canvas and the maps both
// use for their nodes and places.

import { untrack } from 'svelte';

/** The most entities a selection holds; a larger one keeps the first ones given. */
export const SELECTION_CAP = 20_000;

export class SharedSelection {
  /** Replaced, never changed in place, so that readers see every change. */
  ids = $state<ReadonlySet<string>>(new Set());
  /** Who made it, as the selection bar says it: "the map", "the graph". */
  source = $state('');
  /** How many entities past the cap were left out of the last change. */
  dropped = $state(0);
  /** What a screen reader hears after the last change. */
  announcement = $state('');
  /**
   * Whether the selection filters the views. It is true whenever it holds entities, and
   * also for an empty selection that a source keeps, such as brushes that match none of
   * the drawn entities, so that the views dim everything rather than nothing.
   */
  active = $state(false);

  get size() {
    return this.ids.size;
  }

  has(id: string): boolean {
    return this.ids.has(id);
  }

  private apply(next: Set<string>, source: string, dropped = 0, keepEmpty = false) {
    const active = next.size > 0 || keepEmpty;
    this.ids = next;
    this.active = active;
    this.source = active ? source : '';
    this.dropped = next.size ? dropped : 0;
    this.announcement = this.describe();
  }

  /**
   * Replace the selection. Entities past the cap are left out and counted. With
   * `keepEmpty`, an empty selection stays active and dims every entity.
   */
  set(ids: Iterable<string>, source: string, opts: { keepEmpty?: boolean } = {}) {
    const next = new Set<string>();
    let dropped = 0;
    for (const id of ids) {
      if (next.has(id)) continue;
      if (next.size >= SELECTION_CAP) dropped++;
      else next.add(id);
    }
    this.apply(next, source, dropped, opts.keepEmpty);
  }

  /** Add entities (shift-click, shift-enter). */
  add(ids: Iterable<string>, source: string) {
    // a change reads the selection but must not make an effect that changes it depend on it
    untrack(() => this.addNow(ids, source));
  }

  private addNow(ids: Iterable<string>, source: string) {
    const next = new Set(this.ids);
    let dropped = this.dropped;
    for (const id of ids) {
      if (next.has(id)) continue;
      if (next.size >= SELECTION_CAP) dropped++;
      else next.add(id);
    }
    this.apply(next, this.size ? this.source : source, dropped);
  }

  /** Take entities out of the selection. */
  remove(ids: Iterable<string>) {
    untrack(() => {
      const next = new Set(this.ids);
      for (const id of ids) next.delete(id);
      this.apply(next, this.source, this.dropped);
    });
  }

  /** Add the entities that are not all selected, and remove them when they all are. */
  toggle(ids: Iterable<string>, source: string) {
    const list = [...ids];
    const all = untrack(() => list.length > 0 && list.every((id) => this.ids.has(id)));
    if (all) this.remove(list);
    else this.add(list, source);
  }

  clear() {
    untrack(() => {
      if (!this.ids.size && !this.source && !this.active) return;
      this.apply(new Set(), '');
    });
  }

  /** "41 selected by the map", with the cap named when it cut the selection short. */
  describe(): string {
    const n = this.ids.size;
    const by = this.source ? ` by ${this.source}` : '';
    if (!n) return this.active ? `None selected${by}` : 'Selection cleared';
    const cut = this.dropped ? `, ${this.dropped.toLocaleString('en-US')} more left out` : '';
    return `${n.toLocaleString('en-US')} selected${by}${cut}`;
  }
}

/** The selection of the page: one for the whole UI, since only one dataset is open. */
export const selection = new SharedSelection();
