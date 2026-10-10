// The open items of each dataset's memory review inbox, for the count badges of the
// sidebar's Memory entry and the dataset page. They come from the `review` member of
// `GET /$/memory/{ds}/maintenance`, which the server keeps counted, so a badge never
// loads the inbox itself. The server sends the member only to a caller who may review a
// dataset with memory settings, and the badge shows nothing without it.

import { maintenanceStatus, type ReviewCounts } from './maintenance';

class ReviewCountStore {
  /** The counts by dataset; `null` when the server sends none. */
  counts = $state<Record<string, ReviewCounts | null>>({});
  #pending = new Map<string, Promise<void>>();

  /** The counts of `ds` once loaded, else null. */
  of(ds: string | null | undefined): ReviewCounts | null {
    return ds ? (this.counts[ds] ?? null) : null;
  }

  /** Load the counts of `ds` again. A load of the same dataset in flight is shared. */
  refresh(ds: string | null | undefined): Promise<void> {
    if (!ds) return Promise.resolve();
    const running = this.#pending.get(ds);
    if (running) return running;
    const p = maintenanceStatus(ds)
      .then(
        (s) => {
          this.counts[ds] = s.review ?? null;
        },
        () => {
          // a server without memory maintenance, or a dataset the caller cannot read
          this.counts[ds] = null;
        },
      )
      .finally(() => this.#pending.delete(ds));
    this.#pending.set(ds, p);
    return p;
  }
}

export const reviewCounts = new ReviewCountStore();
