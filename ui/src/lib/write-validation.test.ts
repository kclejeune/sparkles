import { describe, expect, it } from 'vitest';
import type { ValidationCheck } from './api';
import { baselineBadge, checkLine, fallbackText, firstResult } from './write-validation';

const check = (over: Partial<ValidationCheck> = {}): ValidationCheck => ({
  time: '2026-10-02T10:00:00.000Z',
  kind: 'update',
  status: 'passed',
  strategy: 'incremental',
  blocking: 0,
  total: 3,
  focusNodes: 1,
  millis: 2,
  ...over,
});

describe('write-time validation labels', () => {
  it('labels the baseline', () => {
    expect(baselineBadge(null).label).toBe('State unknown');
    const b = {
      commit: 4,
      conforms: false,
      blocking: 2,
      total: 3,
      bySeverity: { violation: 2, warning: 1, info: 0 },
      millis: 5,
    };
    expect(baselineBadge(b)).toEqual({ cls: 'danger', label: '2 blocking results' });
    expect(baselineBadge({ ...b, conforms: true, blocking: 0 }).cls).toBe('ok');
    expect(baselineBadge({ ...b, conforms: null }).label).toBe('State unknown');
  });

  it('summarizes a check', () => {
    expect(checkLine(check())).toBe('Incremental · 1 focus node · 0 blocking of 3 · 2 ms');
    expect(checkLine(check({ strategy: 'full', fallback: 'shapes', focusNodes: 1200 }))).toBe(
      'Full · 1,200 focus nodes · 0 blocking of 3 · 2 ms · full because the shapes changed',
    );
    expect(checkLine(check({ introduced: 1, blocking: 4, fallback: 'sparql' }))).toContain(
      '1 new blocking · 4 blocking of 3 · 2 ms · some shapes in full',
    );
    expect(fallbackText('other')).toBe('other');
    expect(fallbackText(undefined)).toBeNull();
  });

  it('reads the first result of either language', () => {
    expect(
      firstResult(
        check({
          first: {
            sourceShape: { type: 'uri', value: 'http://ex.org/S' },
            focusNode: { type: 'bnode', value: 'b1' },
          },
        }),
      ),
    ).toEqual({ shape: 'http://ex.org/S', node: '_:b1' });
    expect(
      firstResult(
        check({ first: { shape: { type: 'start' }, node: { type: 'uri', value: 'http://x' } } }),
      ),
    ).toEqual({ shape: 'START', node: 'http://x' });
    expect(firstResult(check())).toBeNull();
  });
});
