// The one live Ask thread of the app (C18 §6.2), which the drawer docked on any page and
// the page at /ui/ask both show, so "Open as page" only navigates. It runs each question
// as `POST /{ds}/ask` over server-sent events and folds the events into the thread's
// turns. A page lends its context with `useAskContext`, and the Query page attaches a
// sink so that each answer also opens in a query tab, as the Ask bar's did.

import { onMount } from 'svelte';
import * as api from './api';
import * as askApi from './ask-api';
import { MAIN, onBranch } from './branches';
import {
  askFailure as askFailureText,
  FIRST_STEP,
  loadAskPrefs,
  saveAskPrefs,
  type RunMode,
  type TabQuestion,
  type Turn,
} from './ask';
import {
  chipAbout,
  reduceTurn,
  threadContext,
  threadsFromRecords,
  turnFromRecord,
  turnKey,
  type AskContextChip,
  type AskTarget,
  type Thread,
  type ThreadSummary,
  type ThreadTurn,
} from './ask-thread';

/** The Query page's part in an ask: a tab that follows the turn's events. */
export interface AskSink {
  /**
   * What the request carries from the page: its state, branch, reasoning and row limit,
   * or null when the page's state is not valid, which asks nothing.
   */
  extras(): Partial<askApi.AskRequest> | null;
  /** A question starts; the returned consumer sees its events and its end. */
  begin(o: {
    question: string;
    context: Turn[];
    run: boolean;
    controller: AbortController;
  }): { on(e: askApi.AskEvent): void; end(error?: unknown): void } | null;
  /** Open a turn in a tab of the page, or its Save as example dialog. */
  open(o: {
    query: string;
    question: TabQuestion;
    save: boolean;
    result?: api.SparklesResult;
  }): void;
}

/** One thread per principal and dataset, so that a sign-in never shows another's. */
const STORE_KEY = (principal: string | null, ds: string) =>
  `sparkles.askThread:${JSON.stringify([principal ?? '', ds])}`;

function loadSession(principal: string | null, ds: string): Thread | null {
  try {
    const raw = globalThis.sessionStorage?.getItem(STORE_KEY(principal, ds));
    if (!raw) return null;
    const t = JSON.parse(raw) as Thread;
    if (!Array.isArray(t?.turns)) return null;
    // a reload stops what was running
    for (const turn of t.turns)
      if (turn.status === 'running') {
        turn.status = 'stopped';
        turn.step = undefined;
      }
    return t;
  } catch {
    return null;
  }
}

function saveSession(principal: string | null, ds: string, t: Thread) {
  try {
    // rows can be large: a reload shows them again with Show rows
    const slim = {
      ...t,
      turns: t.turns.map((x) =>
        x.result?.results
          ? { ...x, rowsDropped: true, result: { ...x.result, results: undefined } }
          : x,
      ),
    };
    globalThis.sessionStorage?.setItem(STORE_KEY(principal, ds), JSON.stringify(slim));
  } catch {
    /* ignore */
  }
}

class AskThreadState {
  /** Whether the drawer shows. */
  open = $state(false);
  /** The dataset the thread belongs to. */
  ds = $state<string | null>(null);
  thread = $state<Thread>({ turns: [] });
  /** What the page on screen lends, and whether its chip was removed. */
  pageContext = $state<AskContextChip | null>(null);
  /** The branch and state of the page on screen, with or without a chip. */
  pageTarget = $state<AskTarget | null>(null);
  chipRemoved = $state(false);
  /** The field's text, shared by the drawer and the page. */
  draft = $state('');
  /** What the server says about asking the dataset. */
  status = $state<askApi.AssistantStatus | null>(null);
  /** The run mode and the source of an answer. */
  mode = $state<RunMode>('preview');
  source = $state<askApi.AskMode>('auto');
  /** The history's threads, newest first, when the dataset keeps one. */
  history = $state<ThreadSummary[] | null>(null);
  historyError = $state<string | null>(null);
  /** The principal whose preferences apply. */
  #principal: string | null | undefined = undefined;
  #controller: AbortController | null = null;
  /**
   * Which person and dataset the state belongs to: a response that comes back after
   * either changed is dropped, so that one person's answers never land in another's.
   */
  #gen = 0;
  #sink: AskSink | null = null;
  /** Where the focus was before the drawer opened. */
  #returnFocus: HTMLElement | null = null;
  /** The composer's field, to focus. */
  #focus: (() => void) | null = null;

  get chip(): AskContextChip | null {
    return this.chipRemoved ? null : this.pageContext;
  }
  get running(): boolean {
    return this.thread.turns.some((t) => t.status === 'running');
  }
  get runningTurn(): ThreadTurn | undefined {
    return this.thread.turns.find((t) => t.status === 'running');
  }
  get canAsk(): boolean {
    return !!this.status?.ask;
  }
  get model(): string | null {
    const d = this.status?.draft?.[0];
    return d ? `${d.provider} · ${d.model}` : null;
  }

  /** Follow the open dataset: its status, its thread of this tab and its history. */
  async setDataset(ds: string | null, principal: string | null) {
    const someoneElse = principal !== this.#principal;
    if (someoneElse) {
      this.#principal = principal;
      this.mode = loadAskPrefs(principal).mode;
    }
    if (ds === this.ds && !someoneElse) return;
    // another dataset, or another person: nothing of the last one stays on screen
    this.#gen++;
    this.stop();
    this.ds = ds;
    this.status = null;
    this.history = null;
    this.historyError = null;
    this.draft = '';
    this.thread = (ds && loadSession(principal ?? null, ds)) || { turns: [] };
    if (!ds) return;
    const gen = this.#gen;
    try {
      const s = await askApi.assistantSettings(ds);
      if (gen === this.#gen) this.status = s?.status ?? null;
    } catch {
      if (gen === this.#gen) this.status = null;
    }
  }

  setMode(m: RunMode) {
    this.mode = m;
    const p = loadAskPrefs(this.#principal);
    saveAskPrefs(this.#principal, { ...p, mode: m });
  }

  bindFocus(f: (() => void) | null) {
    this.#focus = f;
  }

  focus() {
    this.#focus?.();
  }

  /** Open the drawer, optionally with a question to ask at once. */
  show(question?: string) {
    if (!this.open && typeof document !== 'undefined')
      this.#returnFocus = document.activeElement as HTMLElement | null;
    this.open = true;
    if (question?.trim()) {
      this.draft = '';
      void this.ask(question.trim());
    } else queueMicrotask(() => this.focus());
  }

  close() {
    this.open = false;
    const el = this.#returnFocus;
    this.#returnFocus = null;
    if (el?.isConnected) queueMicrotask(() => el.focus());
  }

  /** The Query page attaches while it is mounted. */
  attach(sink: AskSink): () => void {
    this.#sink = sink;
    return () => {
      if (this.#sink === sink) this.#sink = null;
    };
  }
  get attached(): boolean {
    return this.#sink != null;
  }

  /** Hand a turn to the attached Query page; false when none is attached. */
  openInQuery(o: Parameters<AskSink['open']>[0]): boolean {
    if (!this.#sink) return false;
    this.#sink.open(o);
    return true;
  }

  #persist() {
    if (this.ds) saveSession(this.#principal ?? null, this.ds, this.thread);
  }

  newThread() {
    this.stop();
    this.thread = { turns: [] };
    this.chipRemoved = false;
    this.draft = '';
    this.#persist();
    queueMicrotask(() => this.focus());
  }

  stop() {
    this.#controller?.abort();
    this.#controller = null;
  }

  /**
   * Ask a question in the thread. `retry` asks an earlier turn again in place, for a
   * clarification, Try harder or another source.
   */
  async ask(
    question: string,
    o: {
      retry?: ThreadTurn;
      clarification?: { id: string; value: string };
      tryHarder?: string;
      mode?: askApi.AskMode;
    } = {},
  ) {
    const ds = this.ds;
    if (!ds || !question.trim()) return;
    // the Query page's state, or the branch and state of the page on screen; a state the
    // Query page cannot read asks nothing
    const extras: Partial<askApi.AskRequest> | null = this.#sink
      ? this.#sink.extras()
      : targetBody(o.retry ? { branch: o.retry.branch, at: o.retry.readAt } : this.pageTarget);
    if (!extras) return;
    this.stop();
    const chip = o.retry ? o.retry.chip : (this.chip ?? undefined);
    const earlier = o.retry
      ? this.thread.turns.slice(0, this.thread.turns.indexOf(o.retry))
      : this.thread.turns;
    const context = threadContext(earlier, chip);
    const run = this.mode === 'run';
    const summary = !!this.status?.summary;
    const turn: ThreadTurn = {
      key: o.retry?.key ?? turnKey(),
      question,
      chip,
      status: 'running',
      step: FIRST_STEP,
      trail: [],
      at: new Date().toISOString(),
      ...(extras.branch ? { branch: extras.branch } : {}),
      ...(extras.at && !this.#sink ? { readAt: extras.at } : {}),
    };
    if (o.retry) {
      const i = this.thread.turns.indexOf(o.retry);
      this.thread.turns[i] = turn;
    } else this.thread.turns.push(turn);
    // the state proxy of the turn
    const t = this.thread.turns.find((x) => x.key === turn.key)!;
    const controller = new AbortController();
    this.#controller = controller;
    const sink = this.#sink?.begin({ question, context, run, controller }) ?? null;
    const mode = o.mode ?? (this.source === 'auto' ? undefined : this.source);
    const body: askApi.AskRequest = {
      ...extras,
      question,
      run,
      ...(context.length ? { context } : {}),
      ...(this.thread.id ? { thread: this.thread.id } : {}),
      ...(chipAbout(chip) ? { about: chipAbout(chip) } : {}),
      ...(chip?.kind === 'commit' ? { at: `commit:${chip.commit}` } : {}),
      ...(mode ? { mode } : {}),
      ...(o.clarification ? { clarification: o.clarification } : {}),
      ...(o.tryHarder ? { tryHarder: o.tryHarder } : {}),
    };
    let error: unknown;
    const gen = this.#gen;
    try {
      await askApi.askStream(
        ds,
        body,
        (e) => {
          if (gen !== this.#gen) return;
          reduceTurn(t, e, { run, summary });
          if (e.event === 'usage' && e.data.threadId && !this.thread.id)
            this.thread.id = e.data.threadId;
          sink?.on(e);
        },
        controller.signal,
      );
    } catch (e) {
      error = e;
      if (e instanceof DOMException && e.name === 'AbortError') {
        t.status = 'stopped';
      } else {
        const err = e instanceof api.ApiError ? e : null;
        t.failure = failureOf(err, e);
      }
    } finally {
      if (this.#controller === controller) this.#controller = null;
      if (t.status === 'running') t.status = t.failure ? 'failed' : 'done';
      t.step = undefined;
      sink?.end(error);
      if (gen === this.#gen) {
        this.#persist();
        if (this.status?.historyDays) void this.loadHistory();
      }
    }
  }

  /** The history's threads. */
  async loadHistory() {
    const ds = this.ds;
    if (!ds || !this.status?.historyDays) {
      this.history = null;
      return;
    }
    const gen = this.#gen;
    try {
      const list = await askApi.askHistory(ds, undefined, { limit: 500 });
      if (gen === this.#gen) {
        this.history = threadsFromRecords(list);
        this.historyError = null;
      }
    } catch (e) {
      if (gen === this.#gen) this.historyError = api.errorMessage(e);
    }
  }

  /** Reopen a thread of the history, its turns from their stored answers. */
  async reopen(id: string) {
    const ds = this.ds;
    if (!ds) return;
    this.stop();
    const gen = this.#gen;
    const list = await askApi.askHistory(ds, undefined, { thread: id, limit: 1000 });
    if (gen !== this.#gen) return;
    this.thread = { id, turns: list.map(turnFromRecord) };
    this.chipRemoved = true;
    this.#persist();
  }

  /** Delete a thread's records from the history. */
  async deleteThread(id: string) {
    const ds = this.ds;
    if (!ds) return;
    const list = await askApi.askHistory(ds, undefined, { thread: id, limit: 1000 });
    for (const r of list) await askApi.deleteAsk(ds, r.id);
    if (this.thread.id === id) this.newThread();
    await this.loadHistory();
  }

  async feedback(t: ThreadTurn, outcome: askApi.Feedback) {
    if (!this.ds || !t.id) return;
    await askApi.askFeedback(this.ds, t.id, outcome);
    t.feedback = outcome;
    this.#persist();
  }

  /** Run a turn's stored query again, read-only, at its commit when that state is kept. */
  async rerun(t: ThreadTurn): Promise<string | null> {
    const ds = this.ds && onBranch(this.ds, t.branch);
    const q = t.result?.query;
    if (!ds || !q) return null;
    t.rerun = 'loading';
    const commit = t.result?.commit ?? t.commit;
    try {
      let note: string | null = null;
      let res: api.SparklesResult;
      try {
        res = await api.query(ds, q, {
          send: 200,
          ...(commit != null ? { at: `commit:${commit}` } : {}),
        });
      } catch (e) {
        // only a state the server no longer keeps falls back to the current one
        const gone = e instanceof api.ApiError && (e.status === 410 || e.code === 'history-gone');
        if (commit == null || !gone) throw e;
        res = await api.query(ds, q, { send: 200 });
        note = `Commit ${commit} is no longer kept, so these rows are from the current state.`;
      }
      t.result = { ...(t.result ?? {}), results: res };
      t.rerun = 'done';
      t.rowsDropped = false;
      return note;
    } catch (e) {
      t.rerun = 'failed';
      throw e;
    }
  }
}

function failureOf(err: api.ApiError | null, e: unknown): string {
  const body = err?.body as { resetAt?: unknown } | undefined;
  return askFailureText(
    err?.code ?? '',
    err?.message ?? api.errorMessage(e),
    typeof body?.resetAt === 'string' ? body.resetAt : undefined,
  );
}

/** The branch and state a page reads, which an ask from it reads too. */
function targetBody(c: AskTarget | null | undefined): Partial<askApi.AskRequest> {
  return {
    ...(c?.branch && c.branch !== MAIN ? { branch: c.branch } : {}),
    ...(c?.at ? { at: c.at } : {}),
  };
}

/** Lends the branch and state the page reads to questions asked while it is mounted. */
export function useAskTarget(target: () => AskTarget) {
  $effect(() => {
    const t = target();
    const prev = askThread.pageTarget;
    if (!prev || prev.branch !== t.branch || prev.at !== t.at) askThread.pageTarget = t;
  });
  onMount(() => () => {
    askThread.pageTarget = null;
  });
}

export const askThread = new AskThreadState();

/** Lends the page's context to questions asked while the page is mounted. */
export function useAskContext(ctx: () => AskContextChip | null) {
  $effect(() => {
    const c = ctx();
    const prev = askThread.pageContext;
    const same =
      !!c && !!prev && c.kind === prev.kind && JSON.stringify(c) === JSON.stringify(prev);
    if (!same) {
      askThread.pageContext = c;
      askThread.chipRemoved = false;
    }
  });
  onMount(() => () => {
    askThread.pageContext = null;
  });
}
