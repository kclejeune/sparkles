import { describe, expect, it } from 'vitest';
import { mappingPart, tableKind, tableParams, tableProblem, UPLOAD_ACCEPT } from './upload';

describe('upload table options', () => {
  it('recognizes tables by extension, past a compression extension', () => {
    expect(tableKind('people.csv')).toBe('csv');
    expect(tableKind('People.CSV.gz')).toBe('csv');
    expect(tableKind('a.tsv')).toBe('tsv');
    expect(tableKind('a.tab.zst')).toBe('tsv');
    expect(tableKind('a.ttl')).toBeNull();
    expect(tableKind('csv.ttl')).toBeNull();
    expect(UPLOAD_ACCEPT).toContain('.csv');
    expect(UPLOAD_ACCEPT).toContain('.trix');
  });

  it('sends a SPARQL file as a template and anything else as a CSVW mapping', () => {
    expect(mappingPart('people.rq')).toBe('template');
    expect(mappingPart('people.SPARQL')).toBe('template');
    expect(mappingPart('people.csv-metadata.json')).toBe('mapping');
  });

  it('puts base and key in the query string', () => {
    expect(tableParams({})).toBe('');
    expect(tableParams({ base: ' http://ex.org/p/ ', key: 'id' })).toBe(
      'base=http%3A%2F%2Fex.org%2Fp%2F&key=id',
    );
    expect(tableParams({ base: '', key: '  ' })).toBe('');
  });

  it('explains what the server would refuse', () => {
    const csv = [{ name: 'people.csv' }];
    const rdf = [{ name: 'people.ttl' }];
    // no tables: nothing to check
    expect(tableProblem(rdf, {})).toBeNull();
    // the default mapping needs a base
    expect(tableProblem(csv, {})).toMatch(/needs a base IRI/);
    expect(tableProblem(csv, { base: 'http://ex.org/p/' })).toBeNull();
    expect(tableProblem(csv, { base: 'people/' })).toMatch(/absolute IRI/);
    // a mapping or template makes the base optional, and rules out a key
    const mapping = { name: 'people.csv-metadata.json' };
    expect(tableProblem(csv, { mapping })).toBeNull();
    expect(tableProblem(csv, { mapping, key: 'id' })).toMatch(/default mapping only/);
  });
});
