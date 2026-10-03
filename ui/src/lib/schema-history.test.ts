import { describe, expect, it } from 'vitest';
import type { ProfileProperty, SchemaDiff } from './api';
import { schemaDiffPath, schemaProfilesPath } from './api';
import {
  changePath,
  changeText,
  changeValue,
  coverage,
  diffSummary,
  objectKinds,
  parseAt,
  shortenExpression,
  valuesRange,
} from './schema-history';

const short = (iri: string) =>
  iri.replace('http://www.w3.org/2001/XMLSchema#', 'xsd:').replace('http://ex.org/', 'ex:');

const prop: ProfileProperty = {
  predicate: 'http://ex.org/name',
  instances: 2,
  triples: 3,
  minPerInstance: 1,
  maxPerInstance: 2,
  objects: {
    iri: 1,
    literals: [{ datatype: 'http://www.w3.org/2001/XMLSchema#string', triples: 2 }],
  },
  objectClasses: [],
};

describe('class profiles', () => {
  it('shows coverage, values per instance and kinds', () => {
    expect(coverage(prop, 3)).toBe('2 of 3 (67%)');
    expect(coverage(prop, 0)).toBe('2');
    expect(valuesRange(prop)).toBe('1–2');
    expect(valuesRange({ ...prop, maxPerInstance: 1 })).toBe('1');
    expect(objectKinds(prop, short)).toBe('xsd:string 2 · IRI 1');
  });

  it('builds the request', () => {
    expect(schemaProfilesPath('ds', { graph: 'union', reasoning: false, classes: ['ex:A'] })).toBe(
      '/$/schema/ds/profiles?graph=union&reasoning=false&class=ex%3AA',
    );
    expect(schemaProfilesPath('ds')).toBe('/$/schema/ds/profiles');
  });
});

describe('class expressions', () => {
  it('shortens IRIs', () => {
    expect(
      shortenExpression(
        '<http://ex.org/p> some (<http://ex.org/A> or <http://www.w3.org/2001/XMLSchema#integer>[>= "1"^^<http://www.w3.org/2001/XMLSchema#integer>])',
        short,
      ),
    ).toBe('ex:p some (ex:A or xsd:integer[>= "1"^^xsd:integer])');
  });
});

describe('schema diffs', () => {
  it('accepts states as the server does', () => {
    expect(parseAt(' 42 ')).toBe('42');
    expect(parseAt('commit:7')).toBe('commit:7');
    expect(parseAt('snapshot:before-load')).toBe('snapshot:before-load');
    expect(parseAt('time:2026-01-02T03:04:05Z')).toBe('time:2026-01-02T03:04:05Z');
    for (const bad of ['', 'head~1', 'commit:x', 'time:yesterday', 'snapshot:'])
      expect(parseAt(bad)).toBeNull();
    expect(schemaDiffPath('ds', { from: '3', to: 'commit:5', reasoning: true })).toBe(
      '/$/schema/ds/diff?from=3&to=commit%3A5&reasoning=true',
    );
  });

  it('writes changes', () => {
    expect(changeText({ path: 'observed.instances', from: 1, to: 2 }, short)).toBe(
      'observed.instances 1 → 2',
    );
    expect(
      changeText(
        {
          path: 'declared.superClasses',
          added: ['http://ex.org/A'],
          removed: ['http://ex.org/B'],
        },
        short,
      ),
    ).toBe('declared.superClasses +ex:A −ex:B');
    expect(changeValue({ value: 'Person', lang: 'en' }, short)).toBe('"Person"@en');
    expect(changeValue(null, short)).toBe('–');
    expect(
      changePath(
        'observed.objects.literals[datatype=http://www.w3.org/2001/XMLSchema#integer].languages[lang=en].triples',
        short,
      ),
    ).toBe('observed.objects.literals[xsd:integer].languages[en].triples');
  });

  it('summarizes', () => {
    const d = {
      counts: {
        classesAdded: 1,
        classesRemoved: 0,
        classesChanged: 2,
        predicatesAdded: 0,
        predicatesRemoved: 1,
        predicatesChanged: 0,
      },
      report: [],
    } as unknown as SchemaDiff;
    expect(diffSummary(d)).toBe('1 class added, 2 changed · 1 predicate removed');
    const none = {
      counts: { ...d.counts, classesAdded: 0, classesChanged: 0, predicatesRemoved: 0 },
      report: [],
    } as unknown as SchemaDiff;
    expect(diffSummary(none)).toBe('No changes');
    expect(diffSummary({ ...none, report: [{ path: 'totals.triples', from: 1, to: 2 }] })).toBe(
      'Only totals changed',
    );
  });
});
