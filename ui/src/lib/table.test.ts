import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import { WELL_KNOWN } from './rdf';
import { sortedOrder, sortKey } from './table';

const int = (v: string): Term => ({
  type: 'literal',
  value: v,
  datatype: WELL_KNOWN.xsd + 'integer',
});
const str = (v: string): Term => ({ type: 'literal', value: v });
const iri = (v: string): Term => ({ type: 'uri', value: v });

describe('sortKey', () => {
  it('uses numbers for numeric literals and text otherwise', () => {
    expect(sortKey(int('10'), WELL_KNOWN)).toBe(10);
    expect(sortKey(int('NaN-ish'), WELL_KNOWN)).toBe('NaN-ish');
    expect(sortKey(str('10'), WELL_KNOWN)).toBe('10');
    expect(sortKey(iri(WELL_KNOWN.rdf + 'type'), WELL_KNOWN)).toBe('rdf:type');
    expect(sortKey(null, WELL_KNOWN)).toBeNull();
  });

  it('sorts triple terms by their RDF 1.2 syntax', () => {
    const t: Term = {
      type: 'triple',
      value: { subject: iri('http://a'), predicate: iri('http://p'), object: str('x') },
    };
    expect(sortKey(t, WELL_KNOWN)).toBe('<<( <http://a> <http://p> "x" )>>');
  });
});

describe('sortedOrder', () => {
  const rows: (Term | null)[][] = [[int('10')], [null], [int('9')], [int('100')], [null]];

  it('keeps row order without a sort', () => {
    expect(sortedOrder(rows, null, WELL_KNOWN)).toEqual([0, 1, 2, 3, 4]);
  });

  it('sorts numbers numerically and keeps unbound cells last in both directions', () => {
    expect(sortedOrder(rows, { col: 0, dir: 1 }, WELL_KNOWN)).toEqual([2, 0, 3, 1, 4]);
    expect(sortedOrder(rows, { col: 0, dir: -1 }, WELL_KNOWN)).toEqual([3, 0, 2, 1, 4]);
  });

  it('compares text with numeric collation, case-insensitively, stable on ties', () => {
    const r = [[str('item10')], [str('Item2')], [str('item2')], [str('apple')]];
    expect(sortedOrder(r, { col: 0, dir: 1 }, WELL_KNOWN)).toEqual([3, 1, 2, 0]);
  });

  it('treats a missing column as unbound', () => {
    expect(sortedOrder([[str('b')], [], [str('a')]], { col: 0, dir: 1 }, WELL_KNOWN)).toEqual([
      2, 0, 1,
    ]);
  });
});
