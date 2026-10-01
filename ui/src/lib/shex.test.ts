import { describe, expect, it } from 'vitest';
import { ApiError, type SchemaClass, type SchemaSummary } from './api';
import {
  countsLine,
  exampleDraft,
  exampleMaps,
  expected,
  failureRow,
  readDraft,
  readLang,
  schemaPrefixes,
  shapeFor,
  shapeText,
  shexDraftKey,
  syntaxError,
  topClasses,
  validateLangKey,
} from './shex';

const EX = 'http://ex.org/';
const P = { ex: EX, foaf: 'http://xmlns.com/foaf/0.1/' };

const cls = (iri: string, instances: number, builtin = false): SchemaClass => ({
  iri,
  builtin,
  observed: { instances },
  declared: {
    types: [],
    superClasses: [],
    equivalentClasses: [],
    disjointWith: [],
    labels: [],
    comments: [],
  },
});

const summary = (classes: SchemaClass[]) =>
  ({ classes: { items: classes, total: classes.length, next: null } }) as unknown as SchemaSummary;

describe('storage keys and stored values', () => {
  it('names the per-dataset keys', () => {
    expect(shexDraftKey('ds')).toBe('sparkles.shex.ds');
    expect(validateLangKey('ds')).toBe('sparkles.validate.lang.ds');
  });

  it('reads a draft only with both strings', () => {
    expect(readDraft({ schema: 's', map: 'm' })).toEqual({ schema: 's', map: 'm' });
    expect(readDraft({ schema: 's' })).toBeNull();
    expect(readDraft('s')).toBeNull();
    expect(readDraft(null)).toBeNull();
  });

  it('reads the language, SHACL unless ShEx', () => {
    expect(readLang('shex')).toBe('shex');
    expect(readLang('shacl')).toBe('shacl');
    expect(readLang(undefined)).toBe('shacl');
    expect(readLang('sparql')).toBe('shacl');
  });
});

describe('the example schema', () => {
  it('takes the three classes with the most instances, vocabulary classes left out', () => {
    const s = summary([
      cls(`${EX}Org`, 2),
      cls('http://www.w3.org/2000/01/rdf-schema#Class', 50, true),
      cls(`${EX}Person`, 10),
      cls(`${EX}City`, 3),
      cls(`${EX}Empty`, 0),
      cls(`${EX}Country`, 1),
    ]);
    expect(topClasses(s)).toEqual([`${EX}Person`, `${EX}City`, `${EX}Org`]);
    expect(topClasses(null)).toEqual([]);
  });

  it('names a shape after its class', () => {
    expect(shapeFor(`${EX}Person`, P)).toBe('ex:PersonShape');
    expect(shapeFor('http://other.org/Thing', P)).toBe('<http://other.org/ThingShape>');
  });

  it('declares the prefixes it uses and selects nodes by type', () => {
    const d = exampleDraft([`${EX}Person`, 'http://other.org/Thing'], P);
    expect(d.schema).toContain(`PREFIX ex: <${EX}>`);
    expect(d.schema).not.toContain('PREFIX foaf:');
    expect(d.schema).toContain('ex:PersonShape EXTRA a {\n  a [ex:Person]\n}');
    expect(d.schema).toContain(
      '<http://other.org/ThingShape> EXTRA a {\n  a [<http://other.org/Thing>]\n}',
    );
    expect(d.map).toBe(
      '{FOCUS a ex:Person}@ex:PersonShape,\n' +
        '{FOCUS a <http://other.org/Thing>}@<http://other.org/ThingShape>',
    );
  });

  it('falls back to the people example without classes', () => {
    const d = exampleDraft([], P);
    expect(d.schema).toContain('ex:PersonShape EXTRA a');
    expect(d.map).toBe('{FOCUS a ex:Person}@ex:PersonShape');
  });

  it('offers example maps for the first class', () => {
    const maps = exampleMaps([`${EX}Person`], P);
    expect(maps[0]).toBe('{FOCUS a ex:Person}@ex:PersonShape');
    expect(maps).toContain('{FOCUS a ex:Person}@START');
    expect(maps.at(-1)).toMatch(/^SPARQL """SELECT \?focus WHERE/);
  });
});

describe('labels', () => {
  it('reads the PREFIX declarations of a schema', () => {
    expect(schemaPrefixes(`PREFIX ex: <${EX}> prefix : <http://d/>\nPREFIX foaf:<x:>`)).toEqual({
      ex: EX,
      '': 'http://d/',
      foaf: 'x:',
    });
  });

  it('shows START, prefixed names and blank nodes', () => {
    expect(shapeText({ type: 'start' }, P)).toBe('START');
    expect(shapeText({ type: 'uri', value: `${EX}Person` }, P)).toBe('ex:Person');
    expect(shapeText({ type: 'bnode', value: 'b0' }, P)).toBe('_:b0');
  });

  it('puts cardinalities in words', () => {
    expect(expected(1, 1)).toBe('exactly 1');
    expect(expected(1, null)).toBe('at least 1');
    expect(expected(0, 1)).toBe('at most 1');
    expect(expected(2, 5)).toBe('2 to 5');
  });

  it('turns failures into table rows', () => {
    expect(
      failureRow(
        {
          kind: 'cardinality',
          predicate: `${P.foaf}name`,
          inverse: false,
          min: 1,
          max: 1,
          count: 0,
        },
        P,
      ),
    ).toEqual({
      kind: 'cardinality',
      predicate: 'foaf:name',
      detail: '0 found, exactly 1 expected',
    });
    expect(
      failureRow(
        { kind: 'cardinality', predicate: `${EX}p`, inverse: true, min: 1, max: 2, count: 3 },
        P,
      ).predicate,
    ).toBe('^ex:p');
    const value = {
      type: 'literal' as const,
      value: '200',
      datatype: 'http://www.w3.org/2001/XMLSchema#integer',
    };
    expect(failureRow({ kind: 'facet', value, constraint: 'MAXINCLUSIVE 150' }, P)).toEqual({
      kind: 'facet',
      value,
      detail: 'fails MAXINCLUSIVE 150',
    });
    const mayor = { type: 'uri' as const, value: `${EX}bob` };
    expect(failureRow({ kind: 'closed', predicate: `${EX}mayor`, value: mayor }, P)).toEqual({
      kind: 'closed',
      predicate: 'ex:mayor',
      value: mayor,
      detail: 'not allowed (CLOSED)',
    });
    expect(failureRow({ kind: 'reference', shape: 'ex:Person', value: mayor }, P).detail).toBe(
      'does not conform to ex:Person',
    );
    expect(failureRow({ kind: 'noMatch', detail: 'no partition' }, P)).toEqual({
      kind: 'no match',
      detail: 'no partition',
    });
  });

  it('writes the counts line', () => {
    expect(countsLine({ conformant: 2, nonconformant: 1200 }, 12.4)).toBe(
      '2 conformant · 1,200 nonconformant · 12 ms',
    );
    expect(countsLine({ conformant: 0, nonconformant: 0 }, 1500)).toBe(
      '0 conformant · 0 nonconformant · 1.50 s',
    );
  });
});

describe('syntax errors', () => {
  it('locates a schema syntax error', () => {
    const e = new ApiError(400, 'schema syntax error at line 3, column 7: expected }', {
      line: 3,
      column: 7,
    });
    expect(syntaxError(e)).toEqual({ line: 3, column: 7, schema: true });
  });

  it('tells a shape map error apart', () => {
    const e = new ApiError(400, 'shape map syntax error at line 1, column 2: bad', {
      line: 1,
      column: 2,
    });
    expect(syntaxError(e)?.schema).toBe(false);
  });

  it('ignores other errors', () => {
    expect(syntaxError(new ApiError(400, 'unknown shape ex:S'))).toBeNull();
    expect(syntaxError(new ApiError(500, 'x', { line: 1 }))).toBeNull();
    expect(syntaxError(new Error('x'))).toBeNull();
  });
});
