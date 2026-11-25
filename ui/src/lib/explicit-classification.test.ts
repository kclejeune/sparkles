import { afterEach, describe, expect, it, vi } from 'vitest';
import { ClassifyOptionsState, ExplicitClassification } from './explicit-classification.svelte';

const source = {
  boundStoreId: 'store-a',
  familyId: 'family-a',
  commit: 7,
  inputFingerprint: 'input-a',
  contentFingerprint: 'content-a',
};

function partialResponse(overrides: Record<string, unknown> = {}) {
  return new Response(
    JSON.stringify({
      commit: 7,
      tier: 'disjunctive',
      partial: true,
      complete: false,
      completeForTaken: false,
      consistent: true,
      consistency: 'unknown',
      incompleteCount: 1,
      incomplete: ['http://example.org/A'],
      classes: 1,
      nodeCount: 3,
      unsatisfiableCount: 0,
      unsatisfiable: [],
      excludedCount: 0,
      excluded: [],
      nodes: [
        {
          node: 2,
          classes: ['http://example.org/A'],
          parents: [0],
          parentClasses: ['http://www.w3.org/2002/07/owl#Thing'],
          children: 0,
        },
      ],
      classificationSource: source,
      stats: { incremental: false, saturationMs: 12 },
      ...overrides,
    }),
    { status: 200, headers: { 'Content-Type': 'application/json' } },
  );
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

afterEach(() => vi.unstubAllGlobals());

describe('ExplicitClassification state hygiene', () => {
  it('clears a prior partial result immediately on rerun and leaves no stale state on 408 timeout', async () => {
    let call = 0;
    const second = deferred<Response>();
    vi.stubGlobal('fetch', () => {
      call++;
      if (call === 1) return Promise.resolve(partialResponse());
      return second.promise;
    });

    const state = new ExplicitClassification();
    state.partial = true;
    state.deadline = '0.2';
    await state.run('uni', { graph: null, imports: true }, 7);
    expect(state.classification?.partial).toBe(true);
    expect(state.error).toBeNull();

    // Disable partial and start a rerun that times out with HTTP 408.
    state.partial = false;
    const rerun = state.run('uni', { graph: null, imports: true }, 7);
    expect(state.classifying).toBe(true);
    expect(state.classification).toBeNull();

    second.resolve(
      new Response(JSON.stringify({ error: 'classification timed out' }), {
        status: 408,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
    await rerun;

    expect(state.classifying).toBe(false);
    expect(state.classification).toBeNull();
    expect(state.error).toMatch(/timed out/i);
  });

  it('drops a delayed classification response across a same-commit import scope change', async () => {
    const inFlight = deferred<Response>();
    const signals: (AbortSignal | null | undefined)[] = [];
    vi.stubGlobal('fetch', (_url: string, init?: RequestInit) => {
      signals.push(init?.signal);
      return inFlight.promise;
    });

    const state = new ExplicitClassification();
    state.partial = true;
    state.deadline = '1';
    const pending = state.run('uni', { graph: null, imports: true }, 7);
    expect(state.classifying).toBe(true);
    await vi.waitFor(() => expect(signals).toHaveLength(1));

    // Import toggle or import-map edit clears classification on the same commit.
    state.clear();
    expect(signals[0]?.aborted).toBe(true);
    expect(state.classifying).toBe(false);
    expect(state.classification).toBeNull();

    // Even if the old promise resolves late, it cannot repopulate state.
    inFlight.resolve(partialResponse());
    await pending;
    expect(state.classification).toBeNull();
    expect(state.error).toBeNull();
    expect(state.labels.size).toBe(0);
  });

  it('resets partial, deadline and expansion options on dataset switch', () => {
    const state = new ExplicitClassification();
    state.partial = true;
    state.deadline = '0.4ms';
    state.expansion = 'eager';
    expect(state.options).toEqual({ partial: true, timeoutMs: 0.4, expansion: 'eager' });

    state.clear();
    expect(state.partial).toBe(true);
    expect(state.deadline).toBe('0.4ms');
    expect(state.expansion).toBe('eager');

    state.resetOptions();
    expect(state.partial).toBe(false);
    expect(state.deadline).toBe('');
    expect(state.expansion).toBe('cautious');
    expect(state.options).toEqual({
      partial: undefined,
      timeoutMs: undefined,
      expansion: 'cautious',
    });
  });
});

describe('ClassifyOptionsState synchronization', () => {
  it('initializes with exact sub-millisecond deadline precision and syncs parent updates and clears', () => {
    const opts = new ClassifyOptionsState('uni', null, {
      partial: true,
      timeoutMs: 0.4,
      expansion: 'eager',
    });
    expect(opts.partial).toBe(true);
    expect(opts.deadline).toBe('0.0004');
    expect(opts.parsedDeadline).toEqual({ timeoutMs: 0.4 });
    expect(opts.expansion).toBe('eager');
    expect(opts.requestOptions).toEqual({ partial: true, timeoutMs: 0.4, expansion: 'eager' });

    // Local edit is kept while target, graph and parent options do not change.
    opts.deadline = '1.4ms';
    opts.sync('uni', null, { partial: true, timeoutMs: 0.4, expansion: 'eager' });
    expect(opts.deadline).toBe('1.4ms');
    expect(opts.requestOptions.timeoutMs).toBe(1.4);

    // Clearing parent options (e.g. when tree input becomes invalid) resets stale inherited values.
    opts.sync('uni', null, undefined);
    expect(opts.partial).toBe(false);
    expect(opts.deadline).toBe('');
    expect(opts.expansion).toBe('cautious');
  });

  it('resets local frame edits when transitioning between default-valued options and undefined', () => {
    const defaults = {
      partial: undefined,
      timeoutMs: undefined,
      expansion: 'cautious' as const,
    };
    const opts = new ClassifyOptionsState('uni', null, defaults);
    opts.partial = true;
    opts.deadline = '1.5';
    opts.expansion = 'eager';

    // Re-syncing the same default-valued options keeps local frame edits.
    opts.sync('uni', null, defaults);
    expect(opts.partial).toBe(true);
    expect(opts.deadline).toBe('1.5');
    expect(opts.expansion).toBe('eager');

    // Clearing default-valued options (e.g. invalid tree deadline) resets local edits.
    opts.sync('uni', null, undefined);
    expect(opts.partial).toBe(false);
    expect(opts.deadline).toBe('');
    expect(opts.expansion).toBe('cautious');
    expect(opts.requestOptions).toEqual({
      partial: undefined,
      timeoutMs: undefined,
      expansion: 'cautious',
    });

    // Reverse transition: restoring default-valued options from undefined also resets local edits.
    opts.partial = true;
    opts.deadline = '2.5';
    opts.expansion = 'eager';
    opts.sync('uni', null, defaults);
    expect(opts.partial).toBe(false);
    expect(opts.deadline).toBe('');
    expect(opts.expansion).toBe('cautious');
    expect(opts.requestOptions).toEqual({
      partial: undefined,
      timeoutMs: undefined,
      expansion: 'cautious',
    });
  });

  it('resets local Browse options when switching datasets or graphs with no classifyOptions prop', () => {
    const opts = new ClassifyOptionsState('ds1', null, undefined);
    opts.partial = true;
    opts.deadline = '2.5';
    opts.expansion = 'eager';

    // Same dataset and graph keeps local edits.
    opts.sync('ds1', null, undefined);
    expect(opts.partial).toBe(true);
    expect(opts.deadline).toBe('2.5');

    // Switching dataset resets controls cleanly.
    opts.sync('ds2', null, undefined);
    expect(opts.partial).toBe(false);
    expect(opts.deadline).toBe('');
    expect(opts.expansion).toBe('cautious');

    // Switching graph also resets controls cleanly.
    opts.partial = true;
    opts.sync('ds2', 'urn:g2', undefined);
    expect(opts.partial).toBe(false);
  });
});
