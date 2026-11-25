// State controllers for explicit Workbench classification and frame classification options.
import { SvelteMap } from 'svelte/reactivity';
import { errorMessage } from './api';
import { LabelBatch } from './label-batch';
import {
  classifyOntology,
  formatTimeoutSeconds,
  ontologyLabelsFor,
  parseDeadlineInput,
  type Classification,
  type ClassificationExpansion,
  type ClassifyOptions,
  type Lit,
  type Selection,
} from './ontology';

export class ClassifyOptionsState {
  partial = $state(false);
  deadline = $state('');
  expansion = $state<ClassificationExpansion>('cautious');
  parsedDeadline = $derived(parseDeadlineInput(this.deadline));
  requestOptions = $derived<ClassifyOptions>({
    partial: this.partial || undefined,
    timeoutMs: this.parsedDeadline.timeoutMs,
    expansion: this.expansion,
  });
  #lastKey: string;

  constructor(target: string, graph: string | null, initial?: ClassifyOptions) {
    const snap = ClassifyOptionsState.#snapshot(initial);
    this.partial = snap.partial;
    this.deadline = snap.deadline;
    this.expansion = snap.expansion;
    this.#lastKey = ClassifyOptionsState.#key(target, graph, snap);
  }

  static #snapshot(opts?: ClassifyOptions) {
    return {
      provided: opts != null,
      partial: !!opts?.partial,
      deadline: formatTimeoutSeconds(opts?.timeoutMs) ?? '',
      expansion: (opts?.expansion ?? 'cautious') as ClassificationExpansion,
    };
  }

  static #key(
    target: string,
    graph: string | null,
    snap: {
      provided: boolean;
      partial: boolean;
      deadline: string;
      expansion: ClassificationExpansion;
    },
  ) {
    return JSON.stringify([
      target,
      graph,
      snap.provided,
      snap.partial,
      snap.deadline,
      snap.expansion,
    ]);
  }

  sync(target: string, graph: string | null, opts?: ClassifyOptions) {
    const snap = ClassifyOptionsState.#snapshot(opts);
    const key = ClassifyOptionsState.#key(target, graph, snap);
    if (key === this.#lastKey) return;
    this.#lastKey = key;
    this.partial = snap.partial;
    this.deadline = snap.deadline;
    this.expansion = snap.expansion;
  }
}

export class ExplicitClassification {
  classification = $state<Classification | null>(null);
  error = $state<string | null>(null);
  classifying = $state(false);
  partial = $state(false);
  deadline = $state('');
  expansion = $state<ClassificationExpansion>('cautious');
  parsedDeadline = $derived(parseDeadlineInput(this.deadline));
  options = $derived<ClassifyOptions>({
    partial: this.partial || undefined,
    timeoutMs: this.parsedDeadline.timeoutMs,
    expansion: this.expansion,
  });
  readonly labels = new SvelteMap<string, Lit[]>();
  #seq = 0;
  #abort: AbortController | null = null;
  #scope: { target: string; selection: Selection; commit: number } | null = null;
  #batch: LabelBatch;

  constructor() {
    this.#batch = new LabelBatch(
      (iris) => {
        const scope = this.#scope;
        return scope
          ? ontologyLabelsFor(
              scope.target,
              scope.selection,
              iris,
              undefined,
              `commit:${scope.commit}`,
            )
          : Promise.resolve(new Map());
      },
      (more) => {
        for (const [iri, labels] of more) this.labels.set(iri, labels);
      },
    );
  }

  wantLabel(iri: string) {
    this.#batch.want(iri);
  }

  abort() {
    this.#abort?.abort();
  }

  clear() {
    this.#seq++;
    this.#abort?.abort();
    this.#abort = null;
    this.classification = null;
    this.#scope = null;
    this.#batch.reset();
    this.labels.clear();
    this.classifying = false;
    this.error = null;
  }

  resetOptions() {
    this.partial = false;
    this.deadline = '';
    this.expansion = 'cautious';
  }

  async run(target: string, selection: Selection, commit: number) {
    if (this.classifying) return;
    if (this.parsedDeadline.error) {
      this.error = this.parsedDeadline.error;
      return;
    }
    const my = ++this.#seq;
    const input = { target, selection: { ...selection }, commit };
    const options = { ...this.options };
    this.#abort?.abort();
    const abort = new AbortController();
    this.#abort = abort;
    this.classification = null;
    this.#scope = null;
    this.#batch.reset();
    this.labels.clear();
    this.classifying = true;
    this.error = null;
    try {
      const c = await classifyOntology(
        input.target,
        input.selection,
        abort.signal,
        `commit:${input.commit}`,
        options,
      );
      if (my !== this.#seq) return;
      this.#scope = { ...input, commit: c.commit };
      this.#batch.reset();
      this.labels.clear();
      this.classification = c;
    } catch (e) {
      if (my === this.#seq) {
        this.classification = null;
        this.#scope = null;
        this.#batch.reset();
        this.labels.clear();
        this.error = errorMessage(e);
      }
    } finally {
      if (my === this.#seq) {
        this.classifying = false;
        this.#abort = null;
      }
    }
  }
}
