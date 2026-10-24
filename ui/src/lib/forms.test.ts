import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import {
  changeCount,
  changeSet,
  checkField,
  choiceGroup,
  compilePattern,
  diffValues,
  entryOf,
  entryTerm,
  localZone,
  grouped,
  langMatches,
  parseTermText,
  sameTerm,
  savePlan,
  serverPatterns,
  widgetOf,
  type FormField,
  type FormModel,
} from './forms';
import { XSD } from './xsd';

const EX = 'http://example.org/';
const LANG = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString';
const lit = (value: string, datatype?: string): Term =>
  datatype ? { type: 'literal', value, datatype } : { type: 'literal', value };
const tagged = (value: string, lang: string): Term => ({
  type: 'literal',
  value,
  'xml:lang': lang,
});
const iri = (v: string): Term => ({ type: 'uri', value: EX + v });

function field(over: Partial<FormField> = {}): FormField {
  return {
    id: `<${EX}p>`,
    path: `${EX}p`,
    inverse: false,
    editable: true,
    label: 'P',
    group: null,
    order: null,
    widget: 'text',
    constraints: {},
    sources: [`${EX}S`],
    values: [],
    ...over,
  };
}

const messages = (f: FormField, vs: (Term | null)[]) => checkField(f, vs).map((p) => p.message);

describe('checkField (the browser checks of §2.4)', () => {
  it('counts values for sh:minCount and sh:maxCount', () => {
    const f = field({ constraints: { minCount: 1, maxCount: 2 } });
    expect(messages(f, [])).toEqual(['is required']);
    expect(messages(f, [null])).toEqual(['is required']);
    expect(messages(f, [lit('a')])).toEqual([]);
    expect(messages(f, [lit('a'), lit('b'), lit('c')])).toEqual(['takes at most 2 values']);
    expect(messages(field({ constraints: { maxCount: 1 } }), [lit('a'), lit('b')])).toEqual([
      'takes one value',
    ]);
    expect(messages(field({ constraints: { minCount: 2 } }), [lit('a')])).toEqual([
      'needs at least 2 values',
    ]);
  });

  it('checks sh:datatype by datatype and lexical form', () => {
    const f = field({ constraints: { datatype: `${XSD}integer` } });
    expect(messages(f, [lit('42', `${XSD}integer`)])).toEqual([]);
    expect(messages(f, [lit('4.2', `${XSD}integer`)])).toEqual(['must be a whole number']);
    expect(messages(f, [lit('42')])).toEqual(['must be a whole number']);
    expect(messages(f, [iri('x')])).toEqual(['must be a whole number']);
    const s = field({ constraints: { datatype: `${XSD}string` } });
    expect(messages(s, [lit('x')])).toEqual([]);
    expect(messages(s, [tagged('x', 'en')])).toEqual([`must be xsd:string`]);
    const l = field({ constraints: { datatype: LANG } });
    expect(messages(l, [tagged('x', 'en')])).toEqual([]);
    expect(messages(l, [lit('x')])).toEqual(['must be text with a language']);
  });

  it('checks sh:nodeKind', () => {
    const f = field({ constraints: { nodeKind: 'http://www.w3.org/ns/shacl#IRI' } });
    expect(messages(f, [iri('x')])).toEqual([]);
    expect(messages(f, [lit('x')])).toEqual(['must be an IRI']);
    const g = field({ constraints: { nodeKind: 'http://www.w3.org/ns/shacl#Literal' } });
    expect(messages(g, [iri('x')])).toEqual(['must be a literal']);
  });

  it('checks lengths in code points and patterns with flags', () => {
    const f = field({ constraints: { minLength: 2, maxLength: 3 } });
    expect(messages(f, [lit('a')])).toEqual(['must be at least 2 characters']);
    expect(messages(f, [lit('😀😀')])).toEqual([]);
    expect(messages(f, [lit('abcd')])).toEqual(['must be at most 3 characters']);
    const p = field({ constraints: { pattern: [{ pattern: '^[a-z]+$', flags: 'i' }] } });
    expect(messages(p, [lit('ABC')])).toEqual([]);
    expect(messages(p, [lit('AB1')])).toEqual(['must match ^[a-z]+$']);
    // an IRI is checked by its string value
    const q = field({ constraints: { pattern: [{ pattern: '^http://example' }] } });
    expect(messages(q, [iri('x')])).toEqual([]);
  });

  it('leaves patterns JavaScript cannot compile to the server', () => {
    const bad = { pattern: '(?<!x' };
    expect(compilePattern(bad)).toBeNull();
    const f = field({ constraints: { pattern: [bad] } });
    expect(messages(f, [lit('anything')])).toEqual([]);
    expect(serverPatterns(f)).toEqual(['(?<!x']);
  });

  it('checks numeric and date ranges', () => {
    const f = field({
      constraints: {
        datatype: `${XSD}integer`,
        minInclusive: lit('18', `${XSD}integer`),
        maxExclusive: lit('100', `${XSD}integer`),
      },
    });
    expect(messages(f, [lit('18', `${XSD}integer`)])).toEqual([]);
    expect(messages(f, [lit('17', `${XSD}integer`)])).toEqual(['must be at least 18']);
    expect(messages(f, [lit('100', `${XSD}integer`)])).toEqual(['must be less than 100']);
    const d = field({
      constraints: { datatype: `${XSD}date`, minExclusive: lit('2000-01-01', `${XSD}date`) },
    });
    expect(messages(d, [lit('2000-01-01', `${XSD}date`)])).toEqual([
      'must be more than 2000-01-01',
    ]);
    expect(messages(d, [lit('2000-01-02', `${XSD}date`)])).toEqual([]);
  });

  it('checks sh:in, sh:hasValue, sh:languageIn and sh:uniqueLang', () => {
    const f = field({ constraints: { in: [lit('A'), lit('B')] } });
    expect(messages(f, [lit('A')])).toEqual([]);
    expect(messages(f, [lit('A', `${XSD}string`)])).toEqual([]);
    expect(messages(f, [lit('C')])).toEqual(['must be one of the listed values']);
    const h = field({ constraints: { hasValue: [iri('x')] } });
    expect(messages(h, [iri('y')])).toEqual([`must include ${EX}x`]);
    const l = field({ constraints: { languageIn: ['en', 'fr'] } });
    expect(messages(l, [tagged('a', 'en-GB')])).toEqual([]);
    expect(messages(l, [tagged('a', 'de')])).toEqual(['must have a language of en, fr']);
    expect(messages(l, [lit('a')])).toEqual(['must have a language of en, fr']);
    const u = field({ constraints: { uniqueLang: true } });
    expect(messages(u, [tagged('a', 'en'), tagged('b', 'EN')])).toEqual([
      'has two values in language en',
    ]);
    expect(messages(u, [tagged('a', 'en'), tagged('b', 'fr'), lit('c'), lit('d')])).toEqual([]);
  });

  it('puts value problems on their entry and count problems on the field', () => {
    const f = field({ constraints: { maxCount: 1, minLength: 2 } });
    expect(checkField(f, [lit('a'), null, lit('bb')])).toEqual([
      { index: 0, message: 'must be at least 2 characters' },
      { index: null, message: 'takes one value' },
    ]);
  });
});

describe('widgetOf', () => {
  it('takes the server widget unless a DASH editor overrides it (A8)', () => {
    expect(widgetOf(field({ widget: 'text' }))).toBe('text');
    expect(
      widgetOf(field({ widget: 'text', editor: 'http://datashapes.org/dash#TextAreaEditor' })),
    ).toBe('textarea');
    expect(widgetOf(field({ widget: 'text', editor: 'http://datashapes.org/dash#Unknown' }))).toBe(
      'text',
    );
    // an enum editor without sh:in keeps the constraints' choice
    expect(
      widgetOf(field({ widget: 'text', editor: 'http://datashapes.org/dash#EnumSelectEditor' })),
    ).toBe('text');
    expect(widgetOf(field({ widget: 'wonky' as never }))).toBe('term');
  });
});

describe('sameTerm and langMatches', () => {
  it('compares terms as RDF does', () => {
    expect(sameTerm(lit('a'), lit('a', `${XSD}string`))).toBe(true);
    expect(sameTerm(tagged('a', 'en'), tagged('a', 'EN'))).toBe(true);
    expect(sameTerm(tagged('a', 'en'), lit('a'))).toBe(false);
    expect(sameTerm(iri('a'), lit(`${EX}a`))).toBe(false);
    expect(langMatches('en-GB', 'en')).toBe(true);
    expect(langMatches('eng', 'en')).toBe(false);
  });
});

describe('parseTermText', () => {
  const prefixes = { ex: EX, '': `${EX}base/` };
  it('reads IRIs, prefixed names and literals', () => {
    expect(parseTermText('<http://a/b>', prefixes)).toEqual({ type: 'uri', value: 'http://a/b' });
    expect(parseTermText('https://a/b', prefixes)).toEqual({ type: 'uri', value: 'https://a/b' });
    expect(parseTermText('ex:alice', prefixes)).toEqual(iri('alice'));
    expect(parseTermText(':x', prefixes)).toEqual({ type: 'uri', value: `${EX}base/x` });
    expect(parseTermText('"hi"@en', prefixes)).toEqual(tagged('hi', 'en'));
    expect(parseTermText('"a \\"b\\""', prefixes)).toEqual(lit('a "b"'));
    expect(parseTermText(`"5"^^<${XSD}int>`, prefixes)).toEqual(lit('5', `${XSD}int`));
    expect(parseTermText('"5"^^ex:t', prefixes)).toEqual(lit('5', `${EX}t`));
    expect(parseTermText('42', prefixes)).toEqual(lit('42', `${XSD}integer`));
    expect(parseTermText('4.2', prefixes)).toEqual(lit('4.2', `${XSD}decimal`));
    expect(parseTermText('4e2', prefixes)).toEqual(lit('4e2', `${XSD}double`));
    expect(parseTermText('true', prefixes)).toEqual(lit('true', `${XSD}boolean`));
  });
  it('explains what it cannot read', () => {
    expect(parseTermText('nope:x', prefixes)).toEqual({ error: 'unknown prefix nope:' });
    expect(parseTermText('two words', prefixes)).toHaveProperty('error');
    expect(parseTermText('', prefixes)).toEqual({ error: 'empty' });
  });
});

describe('entryTerm', () => {
  it('types the text by the field', () => {
    const n = field({ constraints: { datatype: `${XSD}integer` } });
    expect(entryTerm(n, 'number', '7', '', {}).term).toEqual(lit('7', `${XSD}integer`));
    expect(entryTerm(n, 'number', '', '', {}).term).toBeNull();
    const l = field({ constraints: { datatype: LANG } });
    expect(entryTerm(l, 'text-lang', 'Hi', 'en', {}).term).toEqual(tagged('Hi', 'en'));
    const dt = field({ constraints: { datatype: `${XSD}dateTime` } });
    expect(entryTerm(dt, 'datetime', '2024-01-02T03:04', '', {}).term).toEqual(
      lit('2024-01-02T03:04:00', `${XSD}dateTime`),
    );
    const s = field({ constraints: { datatype: `${XSD}string` } });
    expect(entryTerm(s, 'text', 'x', '', {}).term).toEqual(lit('x'));
    expect(entryTerm(field(), 'iri', '"x"', '', {})).toEqual({ term: null, error: 'write an IRI' });
  });
});

describe('change sets', () => {
  it('diffs value sets, ignoring order and repeats', () => {
    expect(diffValues([lit('a'), lit('b')], [lit('b'), lit('a'), lit('a')])).toEqual({
      remove: [],
      add: [],
    });
    expect(diffValues([lit('a')], [lit('b')])).toEqual({ remove: [lit('a')], add: [lit('b')] });
  });

  it('lists the changed editable fields with their loaded values', () => {
    const name = field({ id: '<n>', values: [{ term: tagged('Alice', 'en'), graph: null }] });
    const knows = field({
      id: '<k>',
      values: [
        { term: iri('bob'), graph: null },
        { term: iri('carol'), graph: null },
      ],
    });
    const ro = field({ id: '<r>', editable: false, values: [{ term: lit('x'), graph: null }] });
    const model = { fields: [name, knows, ro] } as unknown as FormModel;
    const s = changeSet(model, {
      '<n>': [tagged('Alice', 'en'), tagged('Alice', 'fr')],
      '<k>': [iri('carol'), null],
      '<r>': [],
    });
    expect(s.changes).toEqual([
      { field: '<n>', remove: [], add: [tagged('Alice', 'fr')] },
      { field: '<k>', remove: [iri('bob')], add: [] },
    ]);
    expect(changeCount(s)).toBe(2);
    expect(s.expect).toEqual({ '<n>': [tagged('Alice', 'en')], '<k>': [iri('bob'), iri('carol')] });
    expect(changeSet(model, { '<n>': [tagged('Alice', 'EN')] }).changes).toEqual([
      { field: '<k>', remove: [iri('bob'), iri('carol')], add: [] },
    ]);
  });
});

describe('savePlan', () => {
  const reject = { language: 'shacl' as const, mode: 'reject' as const };
  const warn = { language: 'shacl' as const, mode: 'warn' as const };
  it('waits for a passing check under a rejecting guard', () => {
    expect(savePlan(reject, 0, null)).toBe('check');
    expect(savePlan(reject, 0, { conforms: false, rejected: true, current: false })).toBe('check');
    expect(savePlan(reject, 0, { conforms: false, rejected: true, current: true })).toBe('blocked');
    expect(savePlan(reject, 0, { conforms: true, rejected: false, current: true })).toBe('save');
  });
  it('asks for confirmation with problems otherwise', () => {
    expect(savePlan(null, 0, null)).toBe('save');
    expect(savePlan(warn, 2, null)).toBe('confirm');
    expect(savePlan(warn, 0, { conforms: false, rejected: false, current: true })).toBe('confirm');
    expect(savePlan(warn, 0, { conforms: false, rejected: false, current: false })).toBe('save');
  });
});

describe('grouped', () => {
  it('puts ungrouped fields first and skips empty groups', () => {
    const model = {
      groups: [
        { iri: 'g1', label: 'One', order: 1 },
        { iri: 'g2', label: 'Two', order: 2 },
      ],
      fields: [field({ id: 'a', group: 'g1' }), field({ id: 'b' })],
    } as unknown as FormModel;
    expect(grouped(model).map((g) => [g.group?.label ?? null, g.fields.map((f) => f.id)])).toEqual([
      [null, ['b']],
      ['One', ['a']],
    ]);
  });
});

describe('review fixes', () => {
  it('leaves patterns with the x and q flags to the server', () => {
    expect(compilePattern({ pattern: '^ A B $', flags: 'x' })).toBeNull();
    expect(compilePattern({ pattern: 'a.b', flags: 'q' })).toBeNull();
    expect(compilePattern({ pattern: '^ab$', flags: 'i' })?.test('AB')).toBe(true);
    expect(
      serverPatterns(field({ constraints: { pattern: [{ pattern: 'a b', flags: 'x' }] } })),
    ).toEqual(['a b']);
  });

  it('keeps the timezone of an edited date-time', () => {
    const f = field({ constraints: { datatype: `${XSD}dateTimeStamp` } });
    const e = entryOf(lit('2026-10-24T10:00:00Z', `${XSD}dateTimeStamp`), 'datetime');
    expect(e.text).toBe('2026-10-24T10:00:00');
    expect(e.zone).toBe('Z');
    const r = entryTerm(f, 'datetime', '2026-10-24T11:00', '', {}, e.zone);
    expect(r.term).toEqual(lit('2026-10-24T11:00:00Z', `${XSD}dateTimeStamp`));
    const z = entryOf(lit('2026-10-24T10:00:00+02:00', `${XSD}dateTime`), 'datetime');
    expect(z.zone).toBe('+02:00');
  });

  it('gives a new time stamp the browser timezone and a new date-time none', () => {
    const stamp = field({ constraints: { datatype: `${XSD}dateTimeStamp` } });
    const r = entryTerm(stamp, 'datetime', '2026-10-24T11:00', '', {});
    const zone = localZone('2026-10-24T11:00');
    expect(zone).toMatch(/^(Z|[+-]\d\d:\d\d)$/);
    expect(r.term).toEqual(lit(`2026-10-24T11:00:00${zone}`, `${XSD}dateTimeStamp`));
    const plain = field({ constraints: { datatype: `${XSD}dateTime` } });
    expect(entryTerm(plain, 'datetime', '2026-10-24T11:00', '', {}).term).toEqual(
      lit('2026-10-24T11:00:00', `${XSD}dateTime`),
    );
  });

  it('marks values in graphs the person cannot write', () => {
    expect(entryOf(lit('x'), 'text', undefined, true).readOnly).toBe(true);
    expect(entryOf(lit('x'), 'text').readOnly).toBeUndefined();
  });

  it('renders a scalar select per value and long lists of several values as checkboxes', () => {
    const seven = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map((v) => lit(v));
    const scalar = field({ widget: 'select', constraints: { in: seven, maxCount: 1 } });
    expect(widgetOf(scalar)).toBe('select');
    expect(choiceGroup(scalar, 'select')).toBeNull();
    const several = field({ constraints: { in: seven, maxCount: 3 } });
    expect(choiceGroup(several, 'select')).toBe('checks');
    const few = field({ widget: 'radio', constraints: { in: seven.slice(0, 3) } });
    expect(choiceGroup(few, widgetOf(few))).toBe('radio');
    expect(choiceGroup(field(), 'text')).toBeNull();
  });
});
