// Guards for async work that a newer request can supersede: a response that arrives after
// the user started something newer must not overwrite, clear or delete the newer outcome.

/** The latest execution per key (for example per query tab). */
export class LatestRun {
  #seq = 0;
  #latest = new Map<string, number>();

  /** Start an execution for `key`; the returned check is false once a newer one starts. */
  claim(key: string): () => boolean {
    const id = ++this.#seq;
    this.#latest.set(key, id);
    return () => this.#latest.get(key) === id;
  }
}

/** A generation counter: `bump()` invalidates every generation read before it. */
export class Generation {
  #gen = 1;

  get current(): number {
    return this.#gen;
  }

  bump(): number {
    return ++this.#gen;
  }

  /** False once `bump()` was called after `gen` was read. */
  isCurrent(gen: number): boolean {
    return gen === this.#gen;
  }
}
