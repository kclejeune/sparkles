import { describe, expect, it } from 'vitest';
import type { ReviewFact } from './review-api';
import { factKey, pageList, pageOf, passingKeys, segments, signalText } from './review';

const fact = (o: string, signals: ReviewFact['signals'], confidence?: string): ReviewFact => ({
  s: '<http://example.org/ana>',
  p: '<http://example.org/p>',
  o,
  graph: '<g>',
  shown: { s: 'ex:ana', p: 'ex:p', o, graph: 'g' },
  status: 'unreviewed',
  reifiers: [],
  signals,
  confidence,
});

describe('passingKeys', () => {
  it('selects facts whose span, link and guard pass, whatever their confidence', () => {
    const ok = fact('"a"', { span: 'pass', link: 'pass', guard: 'pass', corroboration: 'none' });
    const twoCandidates = fact(
      '"b"',
      { span: 'pass', link: 'fail', guard: 'pass', corroboration: 'pass' },
      '0.99',
    );
    const noSpan = fact('"c"', {
      span: 'none',
      link: 'pass',
      guard: 'pass',
      corroboration: 'none',
    });
    const keys = passingKeys([ok, twoCandidates, noSpan]);
    expect([...keys]).toEqual([factKey(ok)]);
    // corroboration as a tie-breaker the person can require
    expect(passingKeys([ok], true).size).toBe(0);
  });
});

describe('segments', () => {
  it('splits the text at span boundaries in code points', () => {
    const text = 'é Ana moved. 😀 Kai left.';
    const s = segments(text, [
      { start: 2, end: 11, fact: 0 },
      { start: 15, end: 23, fact: 1 },
    ]);
    expect(s.map((x) => x.text).join('')).toBe(text);
    expect(s.find((x) => x.facts.includes(0))?.text).toBe('Ana moved');
    expect(s.find((x) => x.facts.includes(1))?.text).toBe('Kai left');
  });

  it('ignores spans outside the text', () => {
    expect(segments('abc', [{ start: 2, end: 9, fact: 0 }])).toEqual([
      { text: 'abc', start: 0, end: 3, facts: [] },
    ]);
  });
});

describe('signalText', () => {
  it('words the badges', () => {
    expect(signalText('span', 'none')).toBe('no span');
    expect(signalText('link', 'fail', 2)).toBe('link: 2 candidates');
    expect(signalText('guard', 'pass')).toBe('guard ✓');
    expect(signalText('corroboration', 'none')).toBe('');
  });
});

describe('pageOf', () => {
  const pages = [
    { page: 1, start: 0 },
    { page: 2, start: 120 },
    { page: 4, start: 300 },
  ];
  it('finds the page whose start is the last at or before the offset', () => {
    expect(pageOf(pages, 0)).toBe(1);
    expect(pageOf(pages, 119)).toBe(1);
    expect(pageOf(pages, 120)).toBe(2);
    expect(pageOf(pages, 299)).toBe(2);
    expect(pageOf(pages, 1000)).toBe(4);
  });
  it('has no page without pages', () => {
    expect(pageOf(undefined, 5)).toBeUndefined();
    expect(pageOf([], 5)).toBeUndefined();
  });
  it('lists pages in words', () => {
    expect(pageList([3])).toBe('3');
    expect(pageList([3, 5])).toBe('3 and 5');
    expect(pageList([1, 2, 4])).toBe('1, 2 and 4');
  });
});
