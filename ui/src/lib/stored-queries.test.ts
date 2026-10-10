import { describe, expect, it } from 'vitest';
import {
  buildDefinition,
  formDefaults,
  inputType,
  queryVariables,
  runValues,
  saveProblems,
  validName,
} from './stored-queries';

describe('validName', () => {
  it('follows the server rules', () => {
    expect(validName('adults')).toBe(true);
    expect(validName('a-b_C9')).toBe(true);
    expect(validName('9lives')).toBe(true);
    expect(validName('_x')).toBe(false);
    expect(validName('a b')).toBe(false);
    expect(validName('')).toBe(false);
    expect(validName('a'.repeat(64))).toBe(true);
    expect(validName('a'.repeat(65))).toBe(false);
  });
});

describe('queryVariables', () => {
  it('finds variables in order, once', () => {
    expect(
      queryVariables('SELECT ?name WHERE { ?p ex:name ?name ; ex:age $age FILTER(?age >= ?min) }'),
    ).toEqual(['name', 'p', 'age', 'min']);
  });
  it('skips strings, IRIs and comments', () => {
    expect(
      queryVariables(
        'SELECT * WHERE { ?s <http://e.org/?a=b> "?no" ; ex:p \'?no\' ; ex:q """?no\n""" } # ?no\nFILTER(?s < ?t)',
      ),
    ).toEqual(['s', 't']);
  });
});

describe('parameter form', () => {
  const params = {
    min: { type: 'integer' as const, default: 18 },
    who: { type: 'iri' as const },
    flag: { type: 'boolean' as const, default: true },
    opt: { type: 'string' as const, required: false },
  };
  it('starts from the defaults', () => {
    expect(formDefaults(params)).toEqual({ min: '18', who: '', flag: true, opt: '' });
  });
  it('sends the filled fields and names the missing required ones', () => {
    expect(runValues(params, formDefaults(params))).toEqual({
      values: { min: '18', flag: 'true' },
      missing: ['who'],
    });
    expect(runValues(params, { min: ' 40 ', who: 'ex:a', flag: false, opt: '' })).toEqual({
      values: { min: '40', who: 'ex:a', flag: 'false' },
      missing: [],
    });
  });
  it('picks input types', () => {
    expect(inputType('integer')).toBe('number');
    expect(inputType('boolean')).toBe('checkbox');
    expect(inputType('date')).toBe('date');
    expect(inputType('iri')).toBe('text');
  });
});

describe('saving', () => {
  it('builds the definition', () => {
    expect(
      buildDefinition('SELECT ?x {}', ' People ', [
        { name: 'min', type: 'integer', default: '18', description: '' },
        { name: 'b', type: 'boolean', default: 'false', description: 'A flag' },
        { name: 'who', type: 'iri', default: '', description: '' },
      ]),
    ).toEqual({
      query: 'SELECT ?x {}',
      description: 'People',
      parameters: {
        min: { type: 'integer', default: '18' },
        b: { type: 'boolean', default: false, description: 'A flag' },
        who: { type: 'iri' },
      },
    });
    expect(buildDefinition('ASK {}', '', [])).toEqual({ query: 'ASK {}' });
    expect(buildDefinition('ASK {}', '', [], [' Is it? ', ''])).toEqual({
      query: 'ASK {}',
      questions: ['Is it?'],
    });
  });
  it('checks the fields', () => {
    expect(saveProblems('ok', [])).toEqual([]);
    expect(saveProblems('bad name', [])).toHaveLength(1);
    const row = { type: 'string' as const, default: '', description: '' };
    expect(
      saveProblems('ok', [
        { ...row, name: 'a' },
        { ...row, name: 'a' },
      ]),
    ).toEqual(['A variable is a parameter twice.']);
    expect(saveProblems('ok', [{ ...row, name: 'timeout' }])).toEqual([
      '?timeout has a reserved name.',
    ]);
  });
});
