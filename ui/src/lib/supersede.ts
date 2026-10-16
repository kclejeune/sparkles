// Guards for async work that a newer request can supersede: a response that arrives after
// the user started something newer must not overwrite, clear or delete the newer outcome.

/**
 * The latest execution per key (for example per query tab). Each execution also has an
 * abort signal, which a newer execution of the same key aborts, so that the read it started
 * stops on the server as well as being ignored.
 */
export class LatestRun {
  #seq = 0;
  #latest = new Map<string, number>();
  #controllers = new Map<string, AbortController>();

  /** Start an execution for `key`; the returned check is false once a newer one starts. */
  claim(key: string): () => boolean {
    const id = ++this.#seq;
    this.#latest.set(key, id);
    this.#controllers.get(key)?.abort();
    this.#controllers.set(key, new AbortController());
    return () => this.#latest.get(key) === id;
  }

  /** The abort signal of the latest execution for `key`: pass it to the reads it makes. */
  signal(key: string): AbortSignal | undefined {
    return this.#controllers.get(key)?.signal;
  }

  /** Abort every execution that is still running, for a view that goes away. */
  abortAll(): void {
    for (const c of this.#controllers.values()) c.abort();
  }
}

/** Whether `e` is the rejection of an aborted read. */
export const isAbort = (e: unknown): boolean =>
  e instanceof DOMException && e.name === 'AbortError';

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
