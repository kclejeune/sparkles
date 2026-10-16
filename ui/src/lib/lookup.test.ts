import { describe, expect, it } from 'vitest';
import { chooseCandidate, chosen } from './lookup';

const mapping = JSON.stringify({
  tableSchema: {
    columns: [
      { name: 'id' },
      {
        name: 'team',
        'urn:x-sparkles:lookup': [
          { 'rdf:value': ['Payments', 'payments'], '@id': 'http://e/payments' },
        ],
      },
    ],
  },
});

describe('lookup choices', () => {
  it('adds an entry for the chosen candidate', () => {
    const out = chooseCandidate(mapping, 'team', 'Platform', 'http://e/plat1');
    expect(chosen(out, 'team', 'Platform')).toBe('http://e/plat1');
    expect(chosen(out, 'team', 'payments')).toBe('http://e/payments');
  });

  it('replaces an earlier choice and trims the entry that held the value', () => {
    let out = chooseCandidate(mapping, 'team', 'payments', 'http://e/other');
    expect(chosen(out, 'team', 'payments')).toBe('http://e/other');
    expect(chosen(out, 'team', 'Payments')).toBe('http://e/payments');
    out = chooseCandidate(out, 'team', 'payments', 'http://e/third');
    expect(chosen(out, 'team', 'payments')).toBe('http://e/third');
  });

  it('leaves text that is not a mapping of that column alone', () => {
    expect(chooseCandidate('{', 'team', 'x', 'http://e/1')).toBe('{');
    expect(chooseCandidate(mapping, 'nope', 'x', 'http://e/1')).toBe(mapping);
    expect(chosen('{', 'team', 'x')).toBeUndefined();
  });
});
