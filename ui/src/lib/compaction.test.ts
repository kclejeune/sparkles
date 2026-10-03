import { describe, expect, it } from 'vitest';
import type { CompactionStatus } from './api';
import { compactionSummary, lastCompaction } from './compaction';

const base: CompactionStatus = {
  dataset: 'books',
  enabled: true,
  serverEnabled: true,
  policy: {
    enabled: true,
    minDeltaQuads: 10000,
    deltaRatio: 0.05,
    maxDeltaQuads: 1000000,
    maxDeltaMb: 512,
    maxWalMb: 1024,
    idleSeconds: 300,
    maxAgeSeconds: 86400,
    minIntervalSeconds: 60,
    partial: 'auto' as const,
  },
  own: {},
  state: 'idle',
  measures: {
    generation: 'gen-0003',
    baseQuads: 1000000,
    deltaQuads: 1234,
    deltaBytes: 552832,
    walBytes: 81447,
    idleSeconds: 4,
    oldestChangeSeconds: 120,
    threshold: 60000,
  },
  automaticRuns: 0,
  failures: 0,
};

describe('automatic compaction', () => {
  it('summarizes each state', () => {
    expect(compactionSummary(base)).toEqual({
      label: 'on',
      detail: '1,234 of 60,000 delta quads',
    });
    expect(compactionSummary({ ...base, state: 'off', enabled: false }).detail).toBe(
      'turned off for this dataset',
    );
    expect(
      compactionSummary({ ...base, state: 'off', enabled: false, serverEnabled: false }).detail,
    ).toBe('turned off on the server');
    expect(compactionSummary({ ...base, state: 'due', trigger: 'delta of 60000 quads' })).toEqual({
      label: 'due',
      detail: 'delta of 60000 quads',
    });
    expect(compactionSummary({ ...base, state: 'deferred', deferred: 'history' }).detail).toMatch(
      /retention window/,
    );
    expect(
      compactionSummary({ ...base, state: 'deferred', deferred: 'new', deferredDetail: 'why' })
        .detail,
    ).toBe('why');
    expect(compactionSummary({ ...base, state: 'running' }).label).toBe('running');
  });

  it('describes the last compaction', () => {
    expect(lastCompaction(base)).toBeNull();
    const last = {
      automatic: true,
      startedAt: '2026-10-02T10:00:00.000Z',
      finishedAt: '2026-10-02T10:00:02.000Z',
      seconds: 2.04,
      outcome: 'done' as const,
      lockMs: 3.25,
    };
    expect(lastCompaction({ ...base, last })).toBe(
      'last automatic compaction took 2.04 s, writer lock 3.3 ms',
    );
    expect(
      lastCompaction({
        ...base,
        last: { ...last, mode: 'partial', blocksRewritten: 12, blocksCopied: 2228 },
      }),
    ).toBe('last automatic compaction took 2.04 s, writer lock 3.3 ms, rewrote 12 of 2240 blocks');
    expect(
      lastCompaction({
        ...base,
        last: { ...last, automatic: false, outcome: 'failed', error: 'disk full' },
      }),
    ).toBe('last manual compaction failed: disk full');
  });
});
