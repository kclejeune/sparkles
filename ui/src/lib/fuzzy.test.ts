import { describe, expect, it } from 'vitest';
import { editDistance, fuzzy, rank } from './fuzzy';

const score = (q: string, t: string, k?: string[]) => fuzzy(q, t, k)?.score ?? null;

describe('fuzzy', () => {
  it('ranks exact, prefix, word start and substring matches in that order', () => {
    expect(score('format query', 'Format query')).toBe(1000);
    expect(score('form', 'Format query')).toBe(900);
    expect(score('query', 'Format query')).toBe(799);
    expect(score('mat', 'Format query')).toBe(597);
    expect(fuzzy('form', 'Format query')!.ranges).toEqual([[0, 4]]);
    expect(fuzzy('query', 'Format query')!.ranges).toEqual([[7, 12]]);
  });

  it('does not take a short substring inside a word', () => {
    expect(fuzzy('at', 'Format')).toBeNull();
  });

  it('matches query words at word starts in any order', () => {
    const m = fuzzy('query fo', 'Format query');
    expect(m!.score).toBe(500);
    expect(m!.ranges).toEqual([
      [0, 2],
      [7, 12],
    ]);
  });

  it('matches initials and runs from word starts', () => {
    const m = fuzzy('fq', 'Format query');
    expect(m!.score).toBeGreaterThanOrEqual(300);
    expect(m!.ranges).toEqual([
      [0, 1],
      [7, 8],
    ]);
    expect(fuzzy('nd', 'New dataset…')).not.toBeNull();
    // letters that start no word do not match
    expect(fuzzy('fmt', 'Format query')).toBeNull();
    expect(fuzzy('form', 'Theme: dark')).toBeNull();
  });

  it('allows one typo only in a query of 5 or more letters', () => {
    expect(fuzzy('fromat', 'Format query')).not.toBeNull();
    expect(fuzzy('setings', 'Open dataset settings')).not.toBeNull();
    expect(fuzzy('frm', 'Format query')).toBeNull();
    expect(fuzzy('fomr', 'Format query')).toBeNull();
    expect(fuzzy('fxrmxt', 'Format query')).toBeNull();
    expect(score('fromat', 'Format query')!).toBeLessThan(score('fq', 'Format query')!);
  });

  it('matches keywords only at word starts, without a highlight', () => {
    expect(fuzzy('forms', 'Open dataset settings', ['prefixes', 'forms'])).toEqual({
      score: 800,
      ranges: [],
    });
    expect(fuzzy('orms', 'Open dataset settings', ['forms'])).toBeNull();
    expect(fuzzy('api', 'Open access tokens', ['api keys'])).not.toBeNull();
  });

  it('measures edit distance with swaps and stops past the limit', () => {
    expect(editDistance('form', 'from')).toBe(1);
    expect(editDistance('abc', 'abd')).toBe(1);
    expect(editDistance('abc', 'xyz')).toBe(2);
    expect(editDistance('a', 'abcd')).toBe(2);
  });
});

describe('rank', () => {
  const list = ['Run query', 'Format query', 'Explain query', 'New query tab'];
  const get = (s: string) => ({ id: s, title: s });

  it('orders by score, then length, then label', () => {
    expect(rank(list, 'query', get).map((r) => r.item)).toEqual([
      'Run query',
      'Format query',
      'Explain query',
      'New query tab',
    ]);
  });

  it('breaks near ties toward a recent pick', () => {
    expect(rank(list, 'query', get, new Set(['Explain query']))[0].item).toBe('Explain query');
  });

  it('keeps the order of an empty query', () => {
    expect(rank(list, '', get).map((r) => r.item)).toEqual(list);
  });
});
