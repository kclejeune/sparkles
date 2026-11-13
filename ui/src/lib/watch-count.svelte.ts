// The caller's unread watch matches across every dataset, for the top bar's Watches badge
// (ROADMAP item 619). The layout polls it at most once a minute while the page is visible.
// A caller who cannot watch, such as an anonymous one, gets no badge.

import { listWatches } from './watches';

class WatchCountStore {
  /** The unread matches; null when the caller has no watches or cannot watch. */
  unread = $state<number | null>(null);
  #pending: Promise<void> | null = null;

  /** Load the count again. A load in flight is shared. */
  refresh(): Promise<void> {
    if (this.#pending) return this.#pending;
    this.#pending = listWatches()
      .then(
        (l) => {
          this.unread = l.watches.length ? l.unread : null;
        },
        () => {
          this.unread = null;
        },
      )
      .finally(() => (this.#pending = null));
    return this.#pending;
  }
}

export const watchCount = new WatchCountStore();
