// One displayed class, one immutable result pin. Read retries never compute.
import { errorMessage } from './api';
import {
  CLASSIFICATION_DISPLAY_CAP,
  CLASSIFICATION_PAGE_SIZE,
  projectClassification,
  readKeptClassification,
  type ClassificationFramePart as ClassificationPart,
  type ClassificationResult,
  type ClassificationRow,
  type ClassificationView,
  type KeptClassification,
} from './classification-read';

export type FramePageState = {
  items: ClassificationRow[];
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
const emptyParts = (): Record<ClassificationPart, FramePageState> => ({
  parents: emptyPage(),
  equivalents: emptyPage(),
  sources: emptyPage(),
  excluded: emptyPage(),
});
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
      const reply = await readKeptClassification(view, { resultId }, this.#abort.signal);
      if (epoch !== this.#epoch) return;
      this.#validateStatus(reply, view);
      if (resultId && reply.result && reply.result.resultId !== resultId)
        throw new Error('The kept read changed its selected result pin.');
      if (reply.status === 'available' && reply.result && !this.acceptResult(reply.result))
        throw new Error(
          'The class frame and hierarchy sources differ. Reload this view to select a new source.',
        );
      this.reply = reply;
      if (
        reply.status === 'available' &&
        reply.entity?.present &&
        reply.outcome?.consistency !== 'inconsistent'
      )
        await Promise.all([this.#page('parents', 0), this.#page('equivalents', 0)]);
    } catch (e) {
      if (epoch === this.#epoch) this.error = errorMessage(e);
    } finally {
      if (epoch === this.#epoch) this.loading = false;
    }
  }
  /** Deliberate user action only. This is the controller's sole POST path. */
  async classify() {
    const view = this.#view;
    if (!view || this.computing || this.loading || this.reply?.status === 'running') return;
    const epoch = ++this.#epoch;
    this.#abort.abort();
    this.#abort = new AbortController();
    this.reply = null;
    this.parts = emptyParts();
    this.error = null;
    this.computing = true;
    try {
      const projected = await projectClassification(view, this.#abort.signal);
      if (epoch !== this.#epoch) return;
      const frame = projected.frame;
      if (
        projected.commit !== view.commit ||
        frame.iri !== view.iri ||
        frame.result.classifiedCommit !== view.commit
      )
        throw new Error('The classification changed the displayed source. Read again.');
      const kept = frame.retention.kept;
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
        entity: { iri: view.iri, present: true, unsatisfiable: frame.unsatisfiable },
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
        this.error = errorMessage(e);
      }
    } finally {
      if (epoch === this.#epoch) this.computing = false;
    }
  }
  more(part: ClassificationPart) {
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
  #validateStatus(reply: KeptClassification, view: ClassificationView) {
    if (reply.commit !== view.commit)
      throw new Error('The kept read changed the displayed commit.');
    if (
      reply.status === 'available' &&
      (!reply.result || !reply.outcome || reply.result.classifiedCommit !== view.commit)
    )
      throw new Error('The kept result has no exact source and outcome.');
  }
  async #page(part: ClassificationPart, offset: number) {
    const view = this.#view;
    const pin = this.reply?.result;
    if (!view || !pin || this.parts[part].loading) return;
    const epoch = this.#epoch;
    this.parts[part] = { ...this.parts[part], loading: true, error: null };
    try {
      const reply = await readKeptClassification(
        view,
        { part, offset, resultId: pin.resultId },
        this.#abort.signal,
      );
      if (epoch !== this.#epoch) return;
      this.#validateStatus(reply, view);
      if (reply.status !== 'available' || !reply.result || !sameResult(pin, reply.result)) {
        // Invalidate the entire assembled clause set, including another page that
        // may already have succeeded. A new status read must establish a new pin.
        this.#epoch++;
        this.#abort.abort();
        this.parts = emptyParts();
        this.reply = {
          ...reply,
          status: reply.status === 'available' ? 'mismatch' : reply.status,
          reason: 'resultReplaced',
          result: null,
          outcome: null,
          entity: null,
          page: null,
        };
        this.loading = false;
        return;
      }
      const page = reply.page;
      const about = part === 'parents' || part === 'equivalents' ? view.iri : null;
      if (
        !page ||
        page.part !== part ||
        page.about !== about ||
        page.offset !== offset ||
        page.items.length > CLASSIFICATION_PAGE_SIZE ||
        page.total < offset + page.items.length ||
        (page.nextOffset != null &&
          (page.nextOffset !== offset + page.items.length || page.nextOffset <= offset))
      )
        throw new Error('The kept result returned an invalid page.');
      this.parts[part] = {
        items: [...this.parts[part].items.slice(0, offset), ...page.items],
        total: page.total,
        next: page.nextOffset,
        loaded: true,
        loading: false,
        error: null,
        failedOffset: null,
      };
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
