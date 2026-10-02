import { describe, expect, it } from 'vitest';
import { atInfo } from './api';
import {
  atLabel,
  diffLine,
  diffSummary,
  expiresParam,
  normalizeAt,
  readable,
  validAt,
  validSnapshotName,
} from './history';

describe('at selectors', () => {
  it('normalizes the accepted forms', () => {
    expect(normalizeAt('')).toBeNull();
    expect(normalizeAt(' head ')).toBeNull();
    expect(normalizeAt('42')).toBe('commit:42');
    expect(normalizeAt('commit:007')).toBe('commit:7');
    expect(normalizeAt('snapshot:release-1.0')).toBe('snapshot:release-1.0');
    expect(normalizeAt('time:2026-09-30T14:03:11.482Z')).toBe('time:2026-09-30T14:03:11.482Z');
    expect(normalizeAt('time:2026-09-30T16:03:11+02:00')).toBe('time:2026-09-30T16:03:11+02:00');
  });

  it('rejects the rest', () => {
    for (const s of ['abc', 'commit:-1', 'commit:', 'snapshot:a/b', 'time:yesterday', '-3']) {
      expect(validAt(s), s).toBe(false);
      expect(() => normalizeAt(s)).toThrow(/commit:N/);
    }
    expect(validAt('7')).toBe(true);
  });

  it('checks snapshot names', () => {
    expect(validSnapshotName('nightly_2026-09-30')).toBe(true);
    expect(validSnapshotName('-x')).toBe(false);
    expect(validSnapshotName('a'.repeat(65))).toBe(false);
    expect(validSnapshotName('')).toBe(false);
  });
});

describe('the state a read saw', () => {
  it('reads the headers of a past state', () => {
    const h = new Headers({
      'Sparkles-At': 'commit:42',
      'Sparkles-Commit': '42',
      'Sparkles-Head': '57',
      'Memento-Datetime': 'Wed, 30 Sep 2026 14:03:11 GMT',
    });
    const at = atInfo(h)!;
    expect(at).toEqual({
      selector: 'commit:42',
      commit: 42,
      head: 57,
      historical: true,
      datetime: '2026-09-30T14:03:11.000Z',
    });
    const now = Date.parse('2026-09-30T17:03:11Z');
    expect(atLabel(at, now)).toMatch(/^at commit 42 · .+ · 15 behind head$/);
  });

  it('labels the head and reads without at', () => {
    const at = atInfo(
      new Headers({ 'Sparkles-At': 'head', 'Sparkles-Commit': '57', 'Sparkles-Head': '57' }),
    )!;
    expect(at.historical).toBe(false);
    expect(atLabel(at)).toBe('at commit 57 (head)');
    expect(atInfo(new Headers({ 'Sparkles-Commit': '57' }))).toBeUndefined();
  });

  it('treats commits without the flag as readable', () => {
    expect(readable({})).toBe(true);
    expect(readable({ reconstructable: false })).toBe(false);
  });
});

describe('diffs', () => {
  it('formats changes as signed N-Quads lines', () => {
    expect(
      diffLine({ op: '-', subject: '<urn:a>', predicate: '<urn:p>', object: '"1"', graph: null }),
    ).toBe('− <urn:a> <urn:p> "1" .');
    expect(
      diffLine({
        op: '+',
        subject: '<urn:b>',
        predicate: '<urn:p>',
        object: '2',
        graph: '<urn:g>',
      }),
    ).toBe('+ <urn:b> <urn:p> 2 <urn:g> .');
    expect(diffSummary({ added: 1200, removed: 3 })).toMatch(/^\+1.200 −3$/);
    expect(diffSummary({ added: 0, removed: 0 })).toBe('no change');
  });

  it('reads expiry fields', () => {
    expect(expiresParam('')).toBeNull();
    expect(expiresParam('7d')).toBe('7d');
    expect(expiresParam('2030-01-01T00:00:00Z')).toBe('2030-01-01T00:00:00.000Z');
    expect(() => expiresParam('soon')).toThrow();
  });
});
