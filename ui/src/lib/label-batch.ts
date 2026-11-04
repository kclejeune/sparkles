// Labels read as they are shown (ROADMAP item 725). The ontology editor used to read every
// label of a graph when it opened, which for a large ontology is most of its bytes. It now
// reads the labels of the IRIs that its panels name: each IRI asked for is gathered for a
// moment, and the ones not asked for before are read in one query for each `CHUNK`.

import type { Lit } from './ontology';

/** The IRIs one query reads. */
export const LABEL_CHUNK = 200;

export class LabelBatch {
  #asked = new Set<string>();
  #queue = new Set<string>();
  #timer: ReturnType<typeof setTimeout> | undefined;
  #generation = 0;
  /** The requests made, for tests and the measurements. */
  requests = 0;

  constructor(
    private read: (iris: string[]) => Promise<Map<string, Lit[]>>,
    /** Called with the labels an answer holds, and with an empty list for an IRI it has none for. */
    private apply: (labels: Map<string, Lit[]>) => void,
    private delay = 25,
  ) {}

  /** Whether `iri` has been asked for since the last reset. */
  has(iri: string): boolean {
    return this.#asked.has(iri);
  }

  /** Ask for the label of `iri` unless it was asked for. Safe to call while rendering. */
  want(iri: string): void {
    if (this.#asked.has(iri) || iri.startsWith('_:')) return;
    this.#asked.add(iri);
    this.#queue.add(iri);
    this.#timer ??= setTimeout(() => void this.flush(), this.delay);
  }

  /** Mark IRIs whose labels are known, so that they are not asked for. */
  know(iris: Iterable<string>): void {
    for (const i of iris) this.#asked.add(i);
  }

  /** Read what is queued now. */
  async flush(): Promise<void> {
    this.#timer = undefined;
    const iris = [...this.#queue];
    this.#queue.clear();
    const gen = this.#generation;
    for (let i = 0; i < iris.length; i += LABEL_CHUNK) {
      const part = iris.slice(i, i + LABEL_CHUNK);
      try {
        this.requests++;
        const got = await this.read(part);
        if (gen !== this.#generation) return;
        const out = new Map<string, Lit[]>();
        for (const iri of part) out.set(iri, got.get(iri) ?? []);
        this.apply(out);
      } catch {
        // the names stay local names; an IRI is asked for again after a reset
        if (gen === this.#generation) for (const iri of part) this.#asked.delete(iri);
      }
    }
  }

  /** Forget what was asked for and drop what is in flight, for a new commit or graph. */
  reset(): void {
    this.#generation++;
    this.#asked.clear();
    this.#queue.clear();
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = undefined;
  }
}
