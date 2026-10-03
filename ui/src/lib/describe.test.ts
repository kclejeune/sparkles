import { describe, expect, it } from 'vitest';
import type { DescribeSetting } from './api';
import { describeBody, describeChanged, describeForm, describeSummary } from './describe';

const setting: DescribeSetting = {
  mode: 'cbd',
  labels: false,
  reifiers: false,
  maxTriples: null,
  maxDepth: null,
  source: 'default',
  modes: ['cbd', 'scbd', 'outgoing'],
};

describe('the DESCRIBE setting', () => {
  it('turns the setting into a form and back', () => {
    const f = describeForm({ ...setting, maxTriples: 500 });
    expect(f).toEqual({
      mode: 'cbd',
      labels: false,
      reifiers: false,
      maxTriples: '500',
      maxDepth: '',
    });
    expect(describeBody({ ...f, mode: 'scbd', labels: true, maxDepth: ' 3 ' })).toEqual({
      ok: { mode: 'scbd', labels: true, reifiers: false, maxTriples: 500, maxDepth: 3 },
    });
  });

  it('reads an empty or zero limit as none, and refuses other text', () => {
    const f = describeForm(setting);
    expect(describeBody({ ...f, maxTriples: '0' })).toEqual({
      ok: { mode: 'cbd', labels: false, reifiers: false, maxTriples: null, maxDepth: null },
    });
    expect(describeBody({ ...f, maxTriples: '-1' })).toEqual({
      error: 'Max triples must be a whole number, or empty for no limit',
    });
    expect(describeBody({ ...f, maxDepth: '1.5' })).toHaveProperty('error');
    expect(describeBody({ ...f, maxDepth: '5000000000' })).toEqual({
      error: 'Max depth is too large',
    });
  });

  it('tells a changed form from the saved setting', () => {
    const f = describeForm(setting);
    expect(describeChanged(f, setting)).toBe(false);
    expect(describeChanged({ ...f, reifiers: true }, setting)).toBe(true);
    expect(describeChanged({ ...f, maxTriples: '0' }, setting)).toBe(false);
    expect(describeChanged({ ...f, maxTriples: 'x' }, setting)).toBe(true);
  });

  it('summarizes the setting', () => {
    expect(describeSummary(setting)).toBe('cbd');
    expect(
      describeSummary({ ...setting, mode: 'scbd', labels: true, maxTriples: 500, maxDepth: 2 }),
    ).toBe('scbd, with labels, at most 500 triples, depth 2');
  });
});
