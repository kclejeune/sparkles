import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { poll, type PollEnv } from './poll';

/** A document whose visibility the test sets. */
class FakeDocument extends EventTarget {
  visibilityState: DocumentVisibilityState = 'visible';
  set(state: DocumentVisibilityState) {
    this.visibilityState = state;
    this.dispatchEvent(new Event('visibilitychange'));
  }
}

/** An IntersectionObserver the test drives through `FakeObserver.show`. */
class FakeObserver {
  static last: FakeObserver | null = null;
  observed: Element[] = [];
  disconnected = false;
  constructor(private cb: (entries: { isIntersecting: boolean }[]) => void) {
    FakeObserver.last = this;
  }
  observe(el: Element) {
    this.observed.push(el);
  }
  disconnect() {
    this.disconnected = true;
  }
  static show(on: boolean) {
    FakeObserver.last?.cb([{ isIntersecting: on }]);
  }
}

let doc: FakeDocument;
let env: PollEnv;

beforeEach(() => {
  vi.useFakeTimers();
  doc = new FakeDocument();
  env = {
    document: doc as unknown as PollEnv['document'],
    IntersectionObserver: FakeObserver as unknown as typeof IntersectionObserver,
  };
  FakeObserver.last = null;
});

afterEach(() => vi.useRealTimers());

/** Advance the clock and let the poller's promises settle. */
const advance = (ms: number) => vi.advanceTimersByTimeAsync(ms);

describe('poll', () => {
  it('runs at once, then every interval', async () => {
    const fn = vi.fn(() => true);
    const p = poll(fn, { interval: 1000 }, env);
    await advance(0);
    expect(fn).toHaveBeenCalledTimes(1);
    await advance(3000);
    expect(fn).toHaveBeenCalledTimes(4);
    p.stop();
  });

  it('waits a first interval when not immediate', async () => {
    const fn = vi.fn();
    const p = poll(fn, { interval: 1000, immediate: false }, env);
    await advance(999);
    expect(fn).not.toHaveBeenCalled();
    await advance(1);
    expect(fn).toHaveBeenCalledTimes(1);
    p.stop();
  });

  it('pauses while the page is hidden', async () => {
    const fn = vi.fn(() => true);
    const p = poll(fn, { interval: 1000 }, env);
    await advance(0);
    doc.set('hidden');
    await advance(60_000);
    expect(fn).toHaveBeenCalledTimes(1);
    p.stop();
  });

  it('refreshes at once when the page becomes visible after a missed run', async () => {
    const fn = vi.fn(() => true);
    const p = poll(fn, { interval: 1000 }, env);
    await advance(0);
    doc.set('hidden');
    await advance(5000);
    doc.set('visible');
    await advance(0);
    expect(fn).toHaveBeenCalledTimes(2);
    // then on the interval again
    await advance(1000);
    expect(fn).toHaveBeenCalledTimes(3);
    p.stop();
  });

  it('keeps its schedule after a short absence', async () => {
    const fn = vi.fn(() => true);
    const p = poll(fn, { interval: 1000 }, env);
    await advance(0);
    await advance(300);
    doc.set('hidden');
    await advance(200);
    doc.set('visible');
    await advance(0);
    expect(fn).toHaveBeenCalledTimes(1);
    await advance(500);
    expect(fn).toHaveBeenCalledTimes(2);
    p.stop();
  });

  it('does not start while the page is hidden, and runs once it is shown', async () => {
    doc.visibilityState = 'hidden';
    const fn = vi.fn();
    const p = poll(fn, { interval: 1000 }, env);
    await advance(10_000);
    expect(fn).not.toHaveBeenCalled();
    doc.set('visible');
    await advance(0);
    expect(fn).toHaveBeenCalledTimes(1);
    p.stop();
  });

  it('backs off while nothing changes, and speeds up on activity', async () => {
    let busy = false;
    const at: number[] = [];
    const start = Date.now();
    const fn = vi.fn(() => {
      at.push(Date.now() - start);
      return busy;
    });
    const p = poll(fn, { interval: 100, idle: 1000, maxIdle: 4000 }, env);
    await advance(0);
    await advance(1000 + 2000 + 4000 + 4000);
    expect(at).toEqual([0, 1000, 3000, 7000, 11000]);
    busy = true;
    await advance(4000);
    expect(at.at(-1)).toBe(15000);
    await advance(100);
    expect(at.at(-1)).toBe(15100);
    // quiet again: the backoff starts over
    busy = false;
    await advance(100);
    expect(at.at(-1)).toBe(15200);
    await advance(1000);
    expect(at.at(-1)).toBe(16200);
    p.stop();
  });

  it('counts a failed run as quiet', async () => {
    const fn = vi.fn(async () => {
      throw new Error('offline');
    });
    const p = poll(fn, { interval: 100, idle: 1000, maxIdle: 2000 }, env);
    await advance(0);
    await advance(1000);
    expect(fn).toHaveBeenCalledTimes(2);
    await advance(1999);
    expect(fn).toHaveBeenCalledTimes(2);
    await advance(1);
    expect(fn).toHaveBeenCalledTimes(3);
    p.stop();
  });

  it('refresh runs now and restarts the backoff', async () => {
    const fn = vi.fn(() => false);
    const p = poll(fn, { interval: 100, idle: 1000, maxIdle: 8000 }, env);
    await advance(0);
    await advance(1000 + 2000);
    expect(fn).toHaveBeenCalledTimes(3);
    await p.refresh();
    expect(fn).toHaveBeenCalledTimes(4);
    await advance(1000);
    expect(fn).toHaveBeenCalledTimes(5);
    p.stop();
  });

  it('makes no request while `when` is false', async () => {
    let building = false;
    const fn = vi.fn(() => true);
    const p = poll(fn, { interval: 1000, when: () => building, immediate: false }, env);
    await advance(10_000);
    expect(fn).not.toHaveBeenCalled();
    building = true;
    await advance(1000);
    expect(fn).toHaveBeenCalledTimes(1);
    p.stop();
  });

  it('pauses while the target is off screen', async () => {
    const el = {} as Element;
    const fn = vi.fn(() => true);
    const p = poll(fn, { interval: 1000, target: () => el, immediate: false }, env);
    expect(FakeObserver.last?.observed).toEqual([el]);
    FakeObserver.show(false);
    await advance(10_000);
    expect(fn).not.toHaveBeenCalled();
    FakeObserver.show(true);
    await advance(0);
    expect(fn).toHaveBeenCalledTimes(1);
    p.stop();
  });

  it('stops for good: no timer, no listener, no observer', async () => {
    const el = {} as Element;
    const fn = vi.fn(() => true);
    const remove = vi.spyOn(doc, 'removeEventListener');
    const p = poll(fn, { interval: 1000, target: () => el }, env);
    await advance(0);
    p.stop();
    expect(remove).toHaveBeenCalledWith('visibilitychange', expect.any(Function));
    expect(FakeObserver.last?.disconnected).toBe(true);
    await advance(10_000);
    doc.set('hidden');
    doc.set('visible');
    await advance(10_000);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('a run in flight when stopped schedules nothing', async () => {
    let finish!: () => void;
    const fn = vi.fn(() => new Promise<void>((r) => (finish = r)));
    const p = poll(fn, { interval: 1000 }, env);
    await advance(0);
    p.stop();
    finish();
    await advance(10_000);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});
