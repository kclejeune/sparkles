// The one way the UI refreshes data on a timer. A poller runs only while the page is
// visible and its panel is on screen, polls fast while something is in progress, backs
// off while nothing changes, and stops with the component that started it.

export type PollOptions = {
  /** The delay after a run that reported activity (work in progress, or changed data). */
  interval: number;
  /**
   * The delay after the first run that reported no activity. It doubles after each further
   * quiet run, up to `maxIdle`. Without it the delay stays at `interval`.
   */
  idle?: number;
  /** The longest delay of the backoff (default `idle`, which means no backoff). */
  maxIdle?: number;
  /**
   * Only poll while this holds (a build in progress, say). It is checked at each tick, so a
   * poller whose condition is false keeps a timer but makes no request.
   */
  when?: () => boolean;
  /** Only poll while this element is on screen (an IntersectionObserver watches it). */
  target?: () => Element | null | undefined;
  /** Run once right away (default true). */
  immediate?: boolean;
};

export type Poller = {
  /** Run now (a user action changed something) and restart the backoff. */
  refresh(): Promise<void>;
  /** Stop for good: clears the timer and the listeners. */
  stop(): void;
};

/** What the poller needs from the browser; tests pass their own. */
export type PollEnv = {
  document?: Pick<Document, 'visibilityState' | 'addEventListener' | 'removeEventListener'>;
  IntersectionObserver?: typeof IntersectionObserver;
};

/**
 * Calls `fn` repeatedly. `fn` returns true when something is in progress or the data
 * changed (the next run comes after `interval`), and false or nothing when the data is
 * quiet (the next run comes after the backoff delay). A failed run counts as quiet.
 *
 * While the page is hidden or the target is off screen, the poller pauses. When it comes
 * back, it runs at once if a run fell due while it was away, and otherwise waits for the
 * rest of the delay.
 */
export function poll(
  fn: () => unknown,
  opts: PollOptions,
  env: PollEnv = {
    document: typeof document === 'undefined' ? undefined : document,
    IntersectionObserver:
      typeof IntersectionObserver === 'undefined' ? undefined : IntersectionObserver,
  },
): Poller {
  const idleStart = opts.idle ?? opts.interval;
  const maxIdle = Math.max(opts.maxIdle ?? idleStart, idleStart);
  const doc = env.document;

  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running: Promise<void> | null = null;
  /** The next quiet delay. */
  let idleDelay = idleStart;
  /** When the next run is due (ms since the epoch). */
  let dueAt = Date.now();
  let onScreen = true;

  const visible = () => !doc || doc.visibilityState !== 'hidden';
  const active = () => !stopped && visible() && onScreen;

  function clear() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }

  function schedule(delay: number) {
    clear();
    dueAt = Date.now() + delay;
    if (active()) timer = setTimeout(tick, delay);
  }

  function tick() {
    timer = undefined;
    if (!active()) return;
    if (opts.when && !opts.when()) {
      // nothing to follow: look again later, without a request
      schedule(opts.interval);
      return;
    }
    void run();
  }

  function run(): Promise<void> {
    if (running) return running;
    clear();
    running = (async () => {
      let busy = false;
      try {
        busy = (await fn()) === true;
      } catch {
        busy = false;
      }
      let delay: number;
      if (busy) {
        idleDelay = idleStart;
        delay = opts.interval;
      } else {
        delay = idleDelay;
        idleDelay = Math.min(idleDelay * 2, maxIdle);
      }
      running = null;
      if (!stopped) schedule(delay);
    })();
    return running;
  }

  /** Back on screen, or the page visible again. */
  function resume() {
    if (!active() || running || timer !== undefined) return;
    const left = dueAt - Date.now();
    if (left <= 0) tick();
    else timer = setTimeout(tick, left);
  }

  function onVisibility() {
    if (visible()) resume();
    else clear();
  }
  doc?.addEventListener('visibilitychange', onVisibility);

  let observer: IntersectionObserver | undefined;
  const el = opts.target?.();
  if (el && env.IntersectionObserver) {
    observer = new env.IntersectionObserver((entries) => {
      const e = entries[entries.length - 1];
      if (!e) return;
      onScreen = e.isIntersecting;
      if (onScreen) resume();
      else clear();
    });
    observer.observe(el);
  }

  if (opts.immediate ?? true) resume();
  else schedule(opts.interval);

  return {
    refresh() {
      if (stopped) return Promise.resolve();
      idleDelay = idleStart;
      return running ? running.then(() => run()) : run();
    },
    stop() {
      stopped = true;
      clear();
      doc?.removeEventListener('visibilitychange', onVisibility);
      observer?.disconnect();
    },
  };
}

/** Resolves once the page is visible (at once when it is). */
export function whenVisible(signal?: AbortSignal): Promise<void> {
  if (typeof document === 'undefined' || document.visibilityState !== 'hidden')
    return Promise.resolve();
  return new Promise((resolve, reject) => {
    const done = () => {
      if (document.visibilityState === 'hidden') return;
      document.removeEventListener('visibilitychange', done);
      resolve();
    };
    document.addEventListener('visibilitychange', done);
    signal?.addEventListener(
      'abort',
      () => {
        document.removeEventListener('visibilitychange', done);
        reject(new DOMException('Aborted', 'AbortError'));
      },
      { once: true },
    );
  });
}
