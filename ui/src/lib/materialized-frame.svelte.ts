import { errorMessage } from './api';
import { fetchTerm, type TermPage } from './browse';
import {
  captureView,
  sameView,
  validateMaterialization,
  type InferredView,
  type InferredPins,
} from './inferred-read';

export class MaterializedFrame {
  page = $state<TermPage | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);
  #view: (InferredView & { iri: string }) | null = null;
  #pins: InferredPins | undefined;
  #abort = new AbortController();
  #epoch = 0;

  drop() {
    this.#epoch++;
    this.#abort.abort();
    this.#view = null;
    this.page = null;
    this.loading = false;
    this.error = null;
  }
  reset(view: (InferredView & { iri: string }) | null, pins?: InferredPins) {
    if (
      view &&
      this.#view &&
      sameView(view, this.#view) &&
      view.iri === this.#view.iri &&
      pins === this.#pins
    )
      return;
    this.drop();
    this.#pins = pins;
    if (!view) return;
    this.#view = { ...captureView(view), iri: view.iri };
    void this.refresh();
  }
  async refresh() {
    const view = this.#view;
    if (!view) return;
    const epoch = ++this.#epoch;
    this.#abort.abort();
    this.#abort = new AbortController();
    const pin = this.page?.inferenceSource?.sourceId ?? this.#pins?.materializationId;
    this.page = null;
    this.loading = true;
    this.error = null;
    try {
      const page = await fetchTerm(
        view.target,
        { ...view.selection, at: `commit:${view.commit}`, inferences: true },
        view.iri,
        { lang: view.languages, mentions: 0, incoming: 0, sourceId: pin },
        this.#abort.signal,
      );
      if (epoch !== this.#epoch) return;
      if (page.commit !== view.commit || page.iri !== view.iri)
        throw new Error('The inferred frame changed the displayed entity or revision.');
      const source = validateMaterialization(page.inferenceSource, view, pin);
      if (this.#pins && !this.#pins.acceptMaterialization(source))
        throw new Error(
          'The frame and hierarchy sources differ. Reload this view to select a new source.',
        );
      this.page = page;
    } catch (error) {
      if (epoch === this.#epoch) this.error = errorMessage(error);
    } finally {
      if (epoch === this.#epoch) this.loading = false;
    }
  }
}
