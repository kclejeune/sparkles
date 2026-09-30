import { describe, expect, it } from 'vitest';
import { Generation, LatestRun } from './supersede';

/** A promise with its resolver, to finish requests in a chosen order. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

describe('LatestRun', () => {
  it('only the latest claim per key owns the outcome', () => {
    const runs = new LatestRun();
    const first = runs.claim('tab1');
    expect(first()).toBe(true);
    const second = runs.claim('tab1');
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it('keys are independent', () => {
    const runs = new LatestRun();
    const a = runs.claim('tab1');
    const b = runs.claim('tab2');
    expect(a()).toBe(true);
    expect(b()).toBe(true);
  });

  // Run A is slow, run B (same tab) finishes first; A's late completion must not win.
  it('a superseded completion does not overwrite the newer outcome', async () => {
    const runs = new LatestRun();
    const outcomes: Record<string, string> = {};
    const slow = deferred<string>();
    const fast = deferred<string>();
    const exec = async (res: Promise<string>) => {
      const owns = runs.claim('tab');
      const r = await res;
      if (owns()) outcomes.tab = r;
    };
    const a = exec(slow.promise);
    const b = exec(fast.promise);
    fast.resolve('B');
    await b;
    slow.resolve('A');
    await a;
    expect(outcomes.tab).toBe('B');
  });

  it('a superseded failure does not clear the newer outcome', async () => {
    const runs = new LatestRun();
    const outcomes: Record<string, string> = {};
    const exec = async (res: Promise<string>) => {
      const owns = runs.claim('tab');
      try {
        outcomes.tab = 'running';
        const r = await res;
        if (owns()) outcomes.tab = r;
      } catch {
        if (owns()) delete outcomes.tab;
      }
    };
    const slow = deferred<string>();
    const aborted = slow.promise.then(() => Promise.reject(new Error('aborted')));
    const a = exec(aborted);
    await exec(Promise.resolve('B'));
    slow.resolve('');
    await a;
    expect(outcomes.tab).toBe('B');
  });
});

describe('Generation', () => {
  it('bump invalidates generations read before it', () => {
    const g = new Generation();
    const before = g.current;
    expect(g.isCurrent(before)).toBe(true);
    g.bump();
    expect(g.isCurrent(before)).toBe(false);
    expect(g.isCurrent(g.current)).toBe(true);
  });

  // Explorer: expanding a node, then refocusing (which resets the graph) before the
  // neighbours arrive; the stale response must not add nodes to the new graph.
  it('discards responses that arrive after a reset', async () => {
    const g = new Generation();
    let nodes: string[] = ['old-focus'];
    const neighbours = deferred<string[]>();
    const expand = async () => {
      const gen = g.current;
      const found = await neighbours.promise;
      if (!g.isCurrent(gen)) return;
      nodes.push(...found);
    };
    const pending = expand();
    g.bump();
    nodes = ['new-focus'];
    neighbours.resolve(['stale-neighbour']);
    await pending;
    expect(nodes).toEqual(['new-focus']);
  });
});
