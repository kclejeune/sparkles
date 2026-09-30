import { describe, expect, it } from 'vitest';
import { fmtBytes, fmtCompact, fmtDuration, fmtInt, fmtMs, fmtRelative, formatSse } from './format';

describe('number formatting', () => {
  it('fmtInt rounds and groups, dash for missing', () => {
    expect(fmtInt(1234567.4)).toBe('1,234,567');
    expect(fmtInt(null)).toBe('—');
    expect(fmtInt(Number.NaN)).toBe('—');
  });

  it('fmtCompact switches units at 1000', () => {
    expect(fmtCompact(999)).toBe('999');
    expect(fmtCompact(1000)).toBe('1.0k');
    expect(fmtCompact(12_345)).toBe('12k');
    expect(fmtCompact(2_500_000)).toBe('2.5M');
    expect(fmtCompact(-1)).toBe('—');
  });

  it('fmtBytes uses binary units', () => {
    expect(fmtBytes(0)).toBe('0 B');
    expect(fmtBytes(512)).toBe('512 B');
    expect(fmtBytes(1536)).toBe('1.5 KiB');
    expect(fmtBytes(200 * 1024 * 1024)).toBe('200 MiB');
  });

  it('fmtMs picks a precision by magnitude', () => {
    expect(fmtMs(0.5)).toBe('0.50 ms');
    expect(fmtMs(12.34)).toBe('12.3 ms');
    expect(fmtMs(1234)).toBe('1234 ms');
    expect(fmtMs(12_345)).toBe('12.3 s');
  });

  it('fmtDuration and fmtRelative', () => {
    expect(fmtDuration(59)).toBe('59s');
    expect(fmtDuration(3661)).toBe('1h 1m');
    expect(fmtDuration(90061)).toBe('1d 1h 1m');
    const now = Date.parse('2026-01-01T12:00:00Z');
    expect(fmtRelative('2026-01-01T11:59:58Z', now)).toBe('just now');
    expect(fmtRelative('2026-01-01T11:00:00Z', now)).toBe('1h ago');
    expect(fmtRelative('not a date', now)).toBe('not a date');
    expect(fmtRelative(undefined, now)).toBe('—');
  });
});

describe('formatSse', () => {
  it('keeps short expressions on one line', () => {
    expect(formatSse('(bgp (triple ?s ?p ?o))')).toBe('(bgp (triple ?s ?p ?o))');
  });

  it('breaks long expressions, keeping the operator and scalar arguments together', () => {
    const src = '(project (?s) (filter (> ?o 5) (bgp (triple ?s <http://ex.org/p> ?o))))';
    expect(formatSse(src, 30)).toBe(
      [
        '(project',
        '  (?s)',
        '  (filter',
        '    (> ?o 5)',
        '    (bgp',
        '      (triple ?s <http://ex.org/p> ?o))))',
      ].join('\n'),
    );
  });

  it('does not split strings with parentheses or treat < as an IRI', () => {
    const src = '(filter (< ?x "a (b) c") (table unit))';
    expect(formatSse(src, 1).split('\n')[1].trim()).toBe('(< ?x "a (b) c")');
  });

  it('leaves multi-line input unchanged', () => {
    expect(formatSse('(a\n b)')).toBe('(a\n b)');
  });
});
