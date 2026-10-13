import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import {
  compareRows,
  diffRows,
  diffTriples,
  filterDiff,
  isTooLarge,
  rowAnnouncement,
  rowKey,
} from './rowdiff';

const u = (v: string): Term => ({ type: 'uri', value: v });
const lit = (v: string, datatype?: string, lang?: string): Term => ({
  type: 'literal',
  value: v,
  ...(datatype ? { datatype } : {}),
  ...(lang ? { 'xml:lang': lang } : {}),
});
const b = (v: string): Term => ({ type: 'bnode', value: v });

describe('diffRows', () => {
  it('marks added, removed and kept rows, with removed rows last', () => {
    const left = [[u('ex:alice'), u('ex:bob')]];
    const right = [[u('ex:alice'), u('ex:carol')]];
    const d = diffRows(left, right);
    expect(d.marks).toEqual(['added', 'removed']);
    expect(d.rows).toEqual([right[0], left[0]]);
    expect([d.added, d.removed, d.unchanged]).toEqual([1, 1, 0]);
  });

  it('keeps the right order for kept rows', () => {
    const a = [u('a')];
    const c = [u('c')];
    const d = diffRows([a, c], [c, a]);
    expect(d.rows).toEqual([c, a]);
    expect(d.marks).toEqual(['kept', 'kept']);
  });

  it('compares bags, so a duplicate is one added or removed row', () => {
    const a = [u('a')];
    expect(diffRows([a], [a, a]).marks).toEqual(['kept', 'added']);
    expect(diffRows([a, a], [a]).marks).toEqual(['kept', 'removed']);
    const d = diffRows([a, a, a], [a]);
    expect([d.added, d.removed, d.unchanged]).toEqual([0, 2, 1]);
  });

  it('tells an unbound cell from an empty literal', () => {
    expect(rowKey([null])).not.toBe(rowKey([lit('')]));
    expect(diffRows([[null]], [[lit('')]]).marks).toEqual(['added', 'removed']);
    expect(diffRows([[u('a'), null]], [[u('a'), null]]).marks).toEqual(['kept']);
  });

  it('keeps blank nodes by label', () => {
    expect(diffRows([[b('x')]], [[b('x')]]).marks).toEqual(['kept']);
    expect(diffRows([[b('x')]], [[b('y')]]).marks).toEqual(['added', 'removed']);
  });

  it('tells literals apart by datatype and language', () => {
    const xsdInt = 'http://www.w3.org/2001/XMLSchema#integer';
    const xsdStr = 'http://www.w3.org/2001/XMLSchema#string';
    expect(diffRows([[lit('1')]], [[lit('1', xsdInt)]]).marks).toEqual(['added', 'removed']);
    expect(diffRows([[lit('a', undefined, 'en')]], [[lit('a', undefined, 'de')]]).marks).toEqual([
      'added',
      'removed',
    ]);
    // a plain literal is an xsd:string
    expect(diffRows([[lit('a')]], [[lit('a', xsdStr)]]).marks).toEqual(['kept']);
  });

  it('does not run columns together', () => {
    expect(diffRows([[u('ab'), u('c')]], [[u('a'), u('bc')]]).marks).toEqual(['added', 'removed']);
  });

  it('handles empty sides', () => {
    expect(diffRows([], []).rows).toEqual([]);
    expect(diffRows([], [[u('a')]]).marks).toEqual(['added']);
    expect(diffRows([[u('a')]], []).marks).toEqual(['removed']);
  });
});

describe('compareRows', () => {
  it('gives up past the limit and keeps the counts', () => {
    const rows = Array.from({ length: 5 }, (_, i) => [u(`r${i}`)]);
    const r = compareRows(rows, rows.slice(0, 2), 4);
    expect(isTooLarge(r)).toBe(true);
    expect(r).toMatchObject({ left: 5, right: 2 });
    expect(isTooLarge(compareRows(rows, rows, 5))).toBe(false);
  });
});

describe('diffTriples', () => {
  it('keys triples like rows', () => {
    const t = (o: string): [Term, Term, Term] => [u('s'), u('p'), u(o)];
    const d = diffTriples([t('a'), t('b')], [t('b'), t('c')]);
    expect(d.marks).toEqual(['kept', 'added', 'removed']);
    expect(d.rows).toEqual([t('b'), t('c'), t('a')]);
  });
});

describe('filterDiff', () => {
  const d = diffRows([[u('a')], [u('b')]], [[u('b')], [u('c')]]);
  it('keeps the rows a chip names', () => {
    expect(filterDiff(d, 'all').rows).toHaveLength(3);
    expect(filterDiff(d, 'added').rows).toEqual([[u('c')]]);
    expect(filterDiff(d, 'removed').rows).toEqual([[u('a')]]);
    expect(filterDiff(d, 'unchanged').marks).toEqual(['kept']);
  });
});

describe('rowAnnouncement', () => {
  it('says what changed', () => {
    expect(rowAnnouncement('Commit 3', { added: 1, removed: 1 })).toBe(
      'Commit 3: 1 row added, 1 removed',
    );
    expect(rowAnnouncement('Commit 3', { added: 3, removed: 0 })).toBe('Commit 3: 3 rows added');
    expect(rowAnnouncement('Commit 3', { added: 0, removed: 2 })).toBe('Commit 3: 2 rows removed');
    expect(rowAnnouncement('Commit 3', { added: 0, removed: 0 })).toBe('Commit 3: no rows changed');
    expect(rowAnnouncement('Commit 3', null)).toBe('Commit 3');
  });
});
