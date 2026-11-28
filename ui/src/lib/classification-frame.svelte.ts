// One displayed class, one immutable result pin. Read retries never compute.
import { errorMessage } from './api';
import {
  CLASSIFICATION_DISPLAY_CAP,
  CLASSIFICATION_PAGE_SIZE,
  projectClassification,
  readKeptAxioms,
  readKeptClassification,
  readKeptEntity,
  validateKeptSelection,
  type ClassificationFramePart as ClassificationPart,
  type ClassificationResult,
  type KeptSelection,
  type ClassificationRow,
  type ClassificationView,
  type ClassifyOptions,
  type KeptClassification,
} from './classification-read';
import type { AxiomItem } from './ontology';

type FramePart = ClassificationPart | 'axioms';

export type FramePageState = {
  items: (ClassificationRow | AxiomItem)[];
  total: number;
  next: number | null;
  loaded: boolean;
  loading: boolean;
  error: string | null;
  failedOffset: number | null;
};
const emptyPage = (): FramePageState => ({
  items: [],
  total: 0,
  next: null,
  loaded: false,
  loading: false,
  error: null,
  failedOffset: null,
});
const emptyParts = (): Record<FramePart, FramePageState> => ({
  parents: emptyPage(),
  equivalents: emptyPage(),
  sources: emptyPage(),
  excluded: emptyPage(),
  axioms: emptyPage(),
});
function pageState(
  previous: FramePageState['items'],
  offset: number,
  returnedOffset: number,
  limit: number,
  total: number | null,
  items: FramePageState['items'] | null,
  next: number | null,
): FramePageState {
  if (
    returnedOffset !== offset ||
    limit !== CLASSIFICATION_PAGE_SIZE ||
    !items ||
    items.length > limit ||
    total == null ||
    !Number.isSafeInteger(total) ||
    total < offset + items.length ||
    next !== (total > offset + items.length ? offset + items.length : null) ||
    (next != null && next <= offset)
  )
    throw new Error('The kept result returned an invalid page.');
  return {
    items: [...previous.slice(0, offset), ...items],
    total,
    next,
    loaded: true,
    loading: false,
    error: null,
    failedOffset: null,
  };
}
function sameResult(a: ClassificationResult, b: ClassificationResult) {
  return (
    a.resultId === b.resultId &&
    a.classifiedCommit === b.classifiedCommit &&
    a.boundStoreId === b.boundStoreId &&
    a.familyId === b.familyId &&
    a.inputFingerprint === b.inputFingerprint &&
    a.sourceFingerprint === b.sourceFingerprint &&
    a.contentFingerprint === b.contentFingerprint
  );
}
function sameSource(a: ClassificationView, b: ClassificationView) {
  return (
    a.target === b.target &&
    a.selection.graph === b.selection.graph &&
    a.selection.imports === b.selection.imports &&
    a.iri === b.iri &&
    a.commit === b.commit &&
    a.mode === b.mode
  );
}
export class ClassificationFrame {
  reply = $state<KeptClassification | null>(null);
  parts = $state(emptyParts());
  loading = $state(false);
  computing = $state(false);
  error = $state<string | null>(null);
  isolated = $state(false);
  #view: ClassificationView | null = null;
  #epoch = 0;
  #abort = new AbortController();

  constructor(
    private selectedPin: () => string | undefined = () => undefined,
    private acceptResult: (result: ClassificationResult) => boolean = () => true,
  ) {}

  drop() {
    this.#epoch++;
    this.#abort.abort();
    this.#abort = new AbortController();
    this.#view = null;
    this.reply = null;
    this.parts = emptyParts();
    this.loading = this.computing = this.isolated = false;
    this.error = null;
  }
  async reset(view: ClassificationView | null) {
    const before = this.#view;
    if (
      view &&
      before &&
      sameSource(before, view) &&
      before.languages.length === view.languages.length &&
      before.languages.every((lang, i) => lang === view.languages[i])
    )
      return;
    const pin =
      view && before && !this.isolated && sameSource(before, view)
        ? this.reply?.result?.resultId
        : undefined;
    this.drop();
    if (!view) return;
    // Capture every primitive. Later UI preference/scope mutation cannot retarget
    // an in-flight page or the deliberate projection action.
    this.#view = { ...view, selection: { ...view.selection }, languages: [...view.languages] };
    await this.refresh(pin);
  }
  async refresh(resultId?: string) {
    const view = this.#view;
    if (!view) return;
    resultId ??= this.selectedPin();
    this.#epoch++;
    this.#abort.abort();
    this.#abort = new AbortController();
    const epoch = this.#epoch;
    this.reply = null;
    this.parts = emptyParts();
    this.isolated = false;
    this.loading = true;
    this.error = null;
    try {
      const signal = this.#abort.signal;
      const reply = await readKeptClassification(view, { resultId }, signal);
      if (epoch !== this.#epoch) return;
      validateKeptSelection(reply, view);
      const result = reply.result;
      if (resultId && result && result.resultId !== resultId)
        throw new Error('The kept read changed its selected result pin.');
      if (reply.status === 'available' && reply.entity?.iri !== view.iri)
        throw new Error('The kept read changed the displayed class.');
      if (reply.status === 'available' && result && !this.acceptResult(result))
        throw new Error(
          'The class frame and hierarchy sources differ. Reload this view to select a new source.',
        );
      this.reply = reply;
      const readClauses =
        reply.status === 'available' &&
        result &&
        reply.entity?.present &&
        reply.outcome?.consistency !== 'inconsistent';
      const initialParts = emptyParts();
      let entity = null;
      if (readClauses) {
        // A status miss should stay cheap. The entity route also opens the asserted ontology.
        entity = await readKeptEntity(view, result.resultId, signal);
        if (epoch !== this.#epoch) return;
        if (entity) {
          if (!this.#matchesPage(entity, view, result)) return;
          if (entity.entity?.iri !== view.iri || !entity.entity.present)
            throw new Error('The kept entity read changed the displayed class.');
          // Verify both first pages before publishing either one.
          for (const part of ['parents', 'equivalents'] as const) {
            const page = entity[part];
            if (!page || page.part !== part || page.about !== view.iri)
              throw new Error('The kept entity returned an invalid clause page.');
            initialParts[part] = pageState(
              [],
              0,
              page.offset,
              page.limit,
              page.total,
              page.items,
              page.nextOffset,
            );
          }
        }
      }
      this.parts = initialParts;
      if (readClauses && !entity)
        await Promise.all([this.#page('parents', 0), this.#page('equivalents', 0)]);
    } catch (e) {
      if (epoch === this.#epoch) this.error = errorMessage(e);
    } finally {
      if (epoch === this.#epoch) this.loading = false;
    }
  }
  /** Deliberate user action only. This is the controller's sole POST path. */
  async classify(options?: ClassifyOptions) {
    const view = this.#view;
    if (!view || this.computing || this.loading || this.reply?.status === 'running') return;
    const epoch = ++this.#epoch;
    this.#abort.abort();
    this.#abort = new AbortController();
    this.reply = null;
    this.parts = emptyParts();
    this.isolated = false;
    this.error = null;
    this.computing = true;
    try {
      const projected = await projectClassification(view, this.#abort.signal, options);
      if (epoch !== this.#epoch) return;
      const frame = projected.frame;
      if (
        projected.commit !== view.commit ||
        frame.iri !== view.iri ||
        frame.result.classifiedCommit !== view.commit
      )
        throw new Error('The classification changed the displayed source. Read again.');
      if (frame.outcome.partial && !options?.partial)
        throw new Error('A partial classification was returned without being requested.');
      const kept = frame.retention.kept;
      if (frame.outcome.partial && kept)
        throw new Error('A partial classification cannot be retained for shared reads.');
      if (kept && frame.retention.resultId !== frame.result.resultId)
        throw new Error('The classification returned an invalid retained pin.');
      if (kept && !this.acceptResult(frame.result))
        throw new Error(
          'The class frame and hierarchy sources differ. Reload this view to select a new source.',
        );
      this.isolated = !kept;
      this.reply = {
        commit: projected.commit,
        status: 'available',
        reason: null,
        result: frame.result,
        outcome: frame.outcome,
        entity: {
          iri: view.iri,
          present: true,
          unsatisfiable: frame.unsatisfiable,
          incomplete: frame.incomplete,
        },
        activity: null,
        page: null,
        staleCommit: null,
      };
      for (const part of ['parents', 'equivalents', 'sources', 'excluded'] as const) {
        const page = frame[part];
        if (page.items.length > CLASSIFICATION_PAGE_SIZE || page.items.length > page.total)
          throw new Error('The classification returned an invalid bounded projection.');
        this.parts[part] = {
          items: page.items,
          total: page.total,
          next: kept && page.truncated ? page.items.length : null,
          loaded: true,
          loading: false,
          error: null,
          failedOffset: null,
        };
      }
    } catch (e) {
      if (epoch === this.#epoch) {
        this.reply = null;
        this.parts = emptyParts();
        this.isolated = false;
        this.error = errorMessage(e);
      }
    } finally {
      if (epoch === this.#epoch) this.computing = false;
    }
  }
  more(part: FramePart) {
    if (
      part === 'axioms' &&
      (!this.reply?.entity?.present || this.reply.outcome?.consistency === 'inconsistent')
    )
      return Promise.resolve();
    const state = this.parts[part];
    const offset = state.failedOffset ?? (state.loaded ? state.next : 0);
    if (
      offset == null ||
      state.loading ||
      this.isolated ||
      state.items.length >= CLASSIFICATION_DISPLAY_CAP
    )
      return Promise.resolve();
    return this.#page(part, offset);
  }
  #matchesPage(reply: KeptSelection, view: ClassificationView, pin: ClassificationResult) {
    validateKeptSelection(reply, view);
    if (reply.status === 'available' && reply.result && sameResult(pin, reply.result)) return true;
    // A replaced result cannot be combined with pages already on screen.
    this.#epoch++;
    this.#abort.abort();
    this.parts = emptyParts();
    this.reply = {
      commit: reply.commit,
      status: reply.status === 'available' ? 'mismatch' : reply.status,
      reason: reply.reason ?? 'resultReplaced',
      result: null,
      outcome: null,
      entity: null,
      activity: reply.activity,
      staleCommit: reply.staleCommit,
      page: null,
    };
    this.loading = false;
    return false;
  }
  async #page(part: FramePart, offset: number) {
    const view = this.#view;
    const pin = this.reply?.result;
    if (!view || !pin || this.parts[part].loading) return;
    const epoch = this.#epoch;
    this.parts[part] = { ...this.parts[part], loading: true, error: null };
    try {
      const signal = this.#abort.signal;
      const previous = this.parts[part].items;
      if (part === 'axioms') {
        const reply = await readKeptAxioms(view, pin.resultId, offset, signal);
        if (epoch !== this.#epoch || !this.#matchesPage(reply, view, pin)) return;
        if (reply.entity?.iri !== view.iri || !reply.entity.present)
          throw new Error('The kept axiom listing changed the displayed class.');
        this.parts.axioms = pageState(
          previous,
          offset,
          reply.offset,
          reply.limit,
          reply.total,
          reply.axioms,
          reply.next,
        );
      } else {
        const reply = await readKeptClassification(
          view,
          { part, offset, resultId: pin.resultId },
          signal,
        );
        if (epoch !== this.#epoch || !this.#matchesPage(reply, view, pin)) return;
        const page = reply.page;
        const about = part === 'parents' || part === 'equivalents' ? view.iri : null;
        if (!page || page.part !== part || page.about !== about)
          throw new Error('The kept result returned an invalid page.');
        this.parts[part] = pageState(
          previous,
          offset,
          page.offset,
          page.limit,
          page.total,
          page.items,
          page.nextOffset,
        );
      }
    } catch (e) {
      if (epoch === this.#epoch)
        this.parts[part] = {
          ...this.parts[part],
          loading: false,
          error: errorMessage(e),
          failedOffset: offset,
        };
    }
  }
}
