// The facets' brushes and filter text (facets.ts): the text equals the route's, a brush
// keeps the drawn entities it should, and Apply to query and Use in query write exactly
// the triples, filters and VALUES block they name, against the built browser module.

import { existsSync, readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import type { FacetAnswer, Term } from './api';
import {
  binLabel,
  binsBrush,
  binsFor,
  brushCondition,
  brushedBins,
  brushEdits,
  brushFilter,
  brushText,
  composeEdits,
  edgesOf,
  facetsRequest,
  frameOf,
  inBrush,
  keptBy,
  numberText,
  selectionQuery,
  setQuery,
  toggleValue,
  varFor,
  yearOf,
  type Brush,
} from './facets';
import { applyChanges, filtersOf, lockedOf, triplesOf, type PatternAnswer } from './pattern';
import type { AnyEdit } from './pattern-edits';

const XSD = 'http://www.w3.org/2001/XMLSchema#';
const EX = 'http://ex/';
const int = (n: number): Term => ({ type: 'literal', value: String(n), datatype: `${XSD}integer` });
const date = (s: string): Term => ({ type: 'literal', value: s, datatype: `${XSD}date` });
const str = (s: string): Term => ({ type: 'literal', value: s });

const age: FacetAnswer = {
  property: `${EX}age`,
  kind: 'numeric',
  bins: [
    { from: 20, to: 30, count: 4 },
    { from: 30, to: 40, count: 7 },
    { from: 40, to: 50, count: 2 },
  ],
  missing: 1,
};

describe('brushes', () => {
  it('write the same text as the route', () => {
    const b = binsBrush(age, 2, 1)!;
    expect(b).toEqual({ property: `${EX}age`, kind: 'numeric', range: [30, 50] });
    expect(brushCondition(b, '?age')).toBe('?age >= 30 && ?age < 50');
    expect(brushCondition({ property: `${EX}j`, kind: 'date', range: [2001, 2010] }, '?j')).toBe(
      'YEAR(?j) >= 2001 && YEAR(?j) < 2010',
    );
    expect(
      brushCondition(
        {
          property: `${EX}c`,
          kind: 'values',
          values: [str('FR'), { type: 'uri', value: `${EX}de` }],
        },
        '?c',
      ),
    ).toBe('?c IN ("FR", <http://ex/de>)');
    expect(numberText(0.3)).toBe('0.3');
    expect(numberText(30)).toBe('30');
    expect(brushFilter(b)).toEqual({ property: `${EX}age`, range: [30, 50], kind: 'numeric' });
  });

  it('cover bins and read aloud', () => {
    expect(brushedBins(age, binsBrush(age, 1, 2)!)).toEqual([1, 2]);
    expect(brushedBins(age, undefined)).toBeNull();
    expect(binLabel(age, 0)).toBe('20–29');
    expect(brushText(binsBrush(age, 1, 1)!, 'ex:age')).toBe('ex:age from 30 to 39');
    expect(brushText({ property: 'p', kind: 'values', values: [str('FR'), str('DE')] }, 'c')).toBe(
      'c is FR or DE',
    );
    expect(edgesOf(age)).toEqual([20, 30, 40, 50]);
    expect(frameOf(age)).toEqual({
      property: `${EX}age`,
      kind: 'numeric',
      edges: [20, 30, 40, 50],
    });
    expect(binsFor(41)).toBe(13);
    expect(binsFor(3980)).toBe(40);
    expect(binsFor(4)).toBe(8);
  });

  it('toggle the values of a bar list', () => {
    let b = toggleValue(undefined, 'p', str('FR'));
    expect(b).toEqual({ property: 'p', kind: 'values', values: [str('FR')] });
    b = toggleValue(b!, 'p', str('DE'));
    expect(b && b.kind === 'values' && b.values.length).toBe(2);
    b = toggleValue(b!, 'p', str('FR'));
    b = toggleValue(b!, 'p', str('DE'));
    expect(b).toBeNull();
  });

  it('keep the drawn entities whose values fall inside, as SPARQL compares them', () => {
    const range: Brush = { property: 'a', kind: 'numeric', range: [30, 40] };
    expect(inBrush([int(30)], range)).toBe(true);
    expect(inBrush([int(40)], range)).toBe(false);
    expect(inBrush([str('35')], range)).toBe(false);
    expect(inBrush([int(10), int(35)], range)).toBe(true);
    const years: Brush = { property: 'j', kind: 'date', range: [2001, 2003] };
    expect(yearOf(date('2002-05-01'))).toBe(2002);
    expect(inBrush([date('2002-05-01')], years)).toBe(true);
    expect(inBrush([date('2003-01-01')], years)).toBe(false);
    const values = new Map([
      [
        'a',
        new Map<string, Term[]>([
          ['<x>', [int(31)]],
          ['<y>', [int(50)]],
        ]),
      ],
      ['j', new Map<string, Term[]>([['<x>', [date('2002-01-01')]]])],
    ]);
    expect([...keptBy(['<x>', '<y>', '<z>'], [range], values)]).toEqual(['<x>']);
    expect([...keptBy(['<x>', '<y>'], [range, years], values)]).toEqual(['<x>']);
  });

  it('build requests that hand back the frames', () => {
    const req = facetsRequest({ class: `${EX}P` }, [frameOf(age)], [binsBrush(age, 0, 0)!], {
      at: '42',
    });
    expect(req).toEqual({
      within: { class: `${EX}P` },
      facets: [{ property: `${EX}age`, kind: 'numeric', edges: [20, 30, 40, 50] }],
      filters: [{ property: `${EX}age`, range: [20, 30], kind: 'numeric' }],
      at: '42',
    });
  });

  it('name new variables apart from the query', () => {
    expect(varFor(`${EX}age`, new Set())).toBe('age');
    expect(varFor(`${EX}age`, new Set(['age']))).toBe('age2');
    expect(varFor(`${EX}Date-Joined`, new Set())).toBe('date_Joined');
    expect(varFor('http://ex/2nd', new Set())).toBe('v2nd');
  });

  it('write the queries of Explore', () => {
    const q = setQuery(
      { class: `${EX}Person` },
      [binsBrush(age, 1, 1)!],
      [{ prefix: 'ex', iri: EX }],
    );
    expect(q).toContain('PREFIX ex: <http://ex/>');
    expect(q).toContain('?entity a ex:Person .');
    expect(q).toContain('?entity ex:age ?age .');
    expect(q).toContain('FILTER(?age >= 30 && ?age < 40)');
    const s = selectionQuery(['<http://ex/a>', '<http://ex/b>']);
    expect(s).toContain('VALUES ?entity {\n    <http://ex/a> <http://ex/b>\n  }');
  });
});

const built = new URL('./wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url);

describe.skipIf(!existsSync(built))('Apply to query and Use in query', async () => {
  const bindings = existsSync(built)
    ? await import(/* @vite-ignore */ new URL('./wasm/sparkles_fmt_wasm.js', import.meta.url).href)
    : null;
  bindings?.initSync({ module: readFileSync(built) });
  const parse = (text: string, strict = false) =>
    JSON.parse(
      bindings.pattern(JSON.stringify(strict ? { text, strict } : { text })),
    ) as PatternAnswer;
  const parseStrict = (t: string) => parse(t, true);
  const ok = (text: string) => {
    const a = parse(text);
    if (!a.ok) throw new Error(a.error);
    return a;
  };

  const base = `PREFIX ex: <http://ex/>
# people and their towns
SELECT ?person ?town WHERE {
  ?person a ex:Person ;
    ex:livesIn ?town .
  OPTIONAL { ?person ex:nick ?nick }
}
ORDER BY ?person
`;

  it('adds a triple and a filter per brush and leaves the rest of the text', () => {
    const brushes: Brush[] = [
      { property: `${EX}age`, kind: 'numeric', range: [30, 45] },
      { property: `${EX}country`, kind: 'values', values: [str('FR')] },
    ];
    const changes = composeEdits(base, parse, parseStrict, brushEdits(ok(base), 'person', brushes));
    const out = applyChanges(base, changes);
    const p = ok(out);
    const triples = triplesOf(p).map(
      (t) => `${t.triple.s.text} ${t.triple.p.text} ${t.triple.o.text}`,
    );
    expect(triples).toContain('?person ex:age ?age');
    expect(triples).toContain('?person ex:country ?country');
    expect(filtersOf(p).map((f) => f.filter.text)).toEqual([
      'FILTER(?age >= 30 && ?age < 45)',
      'FILTER(?country IN ("FR"))',
    ]);
    // A5: every byte of the original text is still there, in order
    let at = 0;
    for (const ch of base) {
      at = out.indexOf(ch, at);
      expect(at).toBeGreaterThanOrEqual(0);
      at++;
    }
    expect(out.startsWith('PREFIX ex: <http://ex/>\n# people and their towns\n')).toBe(true);
    expect(out.trimEnd().endsWith('ORDER BY ?person')).toBe(true);
  });

  it('picks variable names the query does not use', () => {
    const q = 'PREFIX ex: <http://ex/>\nSELECT ?age WHERE { ?age ex:age ?x }';
    const edits = brushEdits(ok(q), 'age', [
      { property: `${EX}age`, kind: 'numeric', range: [1, 2] },
    ]);
    const out = applyChanges(q, composeEdits(q, parse, parseStrict, edits));
    expect(out).toContain('?age ex:age ?age2');
    expect(out).toContain('FILTER(?age2 >= 1 && ?age2 < 2)');
  });

  it('writes the selection as a VALUES block first in the WHERE group', () => {
    const e: AnyEdit = {
      kind: 'values-add',
      var: 'person',
      terms: ['<http://ex/a>', '<http://ex/b>'],
    };
    const out = applyChanges(base, composeEdits(base, parse, parseStrict, [e]));
    expect(out).toContain(
      'WHERE {\n  VALUES ?person { <http://ex/a> <http://ex/b> }\n  ?person a ex:Person',
    );
    const p = ok(out);
    expect(lockedOf(p).some((b) => b.text.startsWith('VALUES ?person'))).toBe(true);
    // a long selection breaks into lines
    const many = Array.from({ length: 14 }, (_, i) => `<http://ex/p${i}>`);
    const long = applyChanges(
      base,
      composeEdits(base, parse, parseStrict, [{ kind: 'values-add', var: 'person', terms: many }]),
    );
    expect(long.split('\n').filter((l) => l.includes('<http://ex/p')).length).toBe(3);
    expect(parse(long, true).ok).toBe(true);
    // a term that is not an IRI is refused
    expect(() =>
      composeEdits(base, parse, parseStrict, [
        { kind: 'values-add', var: 'person', terms: ['"x"'] },
      ]),
    ).toThrow(/not an IRI/);
  });
});
