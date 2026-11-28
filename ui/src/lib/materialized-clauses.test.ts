import { describe, expect, it } from 'vitest';
import { materializedClauses, trivialAxiom } from './materialized-clauses';
import type { AxiomItem, AxiomJson } from './ontology';

const OWL = 'http://www.w3.org/2002/07/owl#';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const G = 'urn:x-sparkles:inferred';
const item = (axiom: AxiomJson, ofn: string): AxiomItem => ({
  type: axiom.type,
  ofn,
  axiom,
  graph: G,
});
const name = (iri: string) => iri.replace('urn:', '');
const text = (ts: { text: string }[]) => ts.map((t) => t.text).join('');

describe('materializedClauses', () => {
  it('groups class clauses, marks asserted overlap and counts trivial subsumptions', () => {
    const items = [
      item({ type: 'SubClassOf', subClass: 'urn:A', superClass: 'urn:B' }, 'parent'),
      item({ type: 'EquivalentClasses', classes: ['urn:A', 'urn:C'] }, 'equivalent'),
      item({ type: 'DisjointClasses', classes: ['urn:A', 'urn:D'] }, 'disjoint'),
      item({ type: 'SubClassOf', subClass: 'urn:A', superClass: OWL + 'Thing' }, 'top'),
      item({ type: 'SubClassOf', subClass: 'urn:A', superClass: 'urn:A' }, 'self'),
    ];
    const frame = materializedClauses('Class', 'urn:A', items, new Set(['parent']), name, {});
    expect(
      frame.clauses.map(({ word, rows }) => [word, rows.map((row) => text(row.tokens))]),
    ).toEqual([
      ['SubClassOf', ['B']],
      ['EquivalentTo', ['C']],
      ['DisjointWith', ['D']],
    ]);
    expect(frame.clauses.flatMap(({ rows }) => rows.map((row) => row.alsoAsserted))).toEqual([
      true,
      false,
      false,
    ]);
    expect(frame.other).toEqual([]);
    expect(frame.trivial).toBe(2);
  });

  it('puts an individual’s inferred types and facts in their frame clauses', () => {
    const items = [
      item({ type: 'ClassAssertion', class: 'urn:Artist', individual: 'urn:ada' }, 'CA(Artist)'),
      item({ type: 'ClassAssertion', class: 'urn:Musician', individual: 'urn:ada' }, 'CA(Mus)'),
      item(
        {
          type: 'ObjectPropertyAssertion',
          property: 'urn:relatedTo',
          source: 'urn:ada',
          target: 'urn:quartet',
        },
        'OPA',
      ),
      item({ type: 'ClassAssertion', class: OWL + 'Thing', individual: 'urn:ada' }, 'CA(Thing)'),
      item({ type: 'ClassAssertion', class: RDFS + 'Resource', individual: 'urn:ada' }, 'CA(R)'),
    ];
    const f = materializedClauses(
      'NamedIndividual',
      'urn:ada',
      items,
      new Set(['CA(Mus)']),
      name,
      {},
    );
    expect(f.clauses.map((c) => [c.word, c.rows.map((r) => text(r.tokens))])).toEqual([
      ['Types', ['Artist', 'Musician']],
      ['Facts', ['relatedTo quartet']],
    ]);
    expect(f.clauses[0].rows.map((r) => r.alsoAsserted)).toEqual([false, true]);
    expect(f.trivial).toBe(2);
    expect(f.other).toEqual([]);
  });

  it('counts a property’s relation to itself and to the top property as trivial', () => {
    const items = [
      item({ type: 'SubObjectPropertyOf', subProperty: 'urn:p', superProperty: 'urn:p' }, 'self'),
      item(
        {
          type: 'SubObjectPropertyOf',
          subProperty: 'urn:p',
          superProperty: OWL + 'topObjectProperty',
        },
        'top',
      ),
      item({ type: 'SubObjectPropertyOf', subProperty: 'urn:p', superProperty: 'urn:q' }, 'sup'),
      item({ type: 'SubObjectPropertyOf', subProperty: 'urn:s', superProperty: 'urn:p' }, 'sub'),
      item({ type: 'ObjectPropertyDomain', property: 'urn:p', domain: 'urn:D' }, 'dom'),
    ];
    const f = materializedClauses('ObjectProperty', 'urn:p', items, new Set(), name, {});
    expect(f.clauses.map((c) => [c.id, c.word, c.rows.map((r) => text(r.tokens))])).toEqual([
      ['domains', 'Domain', ['D']],
      ['superProperties', 'SubPropertyOf', ['q']],
      ['subProperties', 'SuperProperty Of', ['s']],
    ]);
    expect(f.trivial).toBe(2);
  });

  it('keeps a chain and an axiom of no clause, and never counts them as trivial', () => {
    const chain: AxiomJson = {
      type: 'SubObjectPropertyOf',
      chain: ['urn:p', 'urn:p'],
      superProperty: 'urn:p',
    };
    expect(trivialAxiom(chain)).toBe(false);
    const items = [
      item(chain, 'chain'),
      item({ type: 'TransitiveObjectProperty', property: 'urn:x' }, 'TransitiveObjectProperty(x)'),
    ];
    const f = materializedClauses('ObjectProperty', 'urn:p', items, new Set(), name, {});
    expect(f.clauses.map((c) => c.id)).toEqual(['chains']);
    expect(f.other.map((r) => r.ofn)).toEqual(['TransitiveObjectProperty(x)']);
    expect(f.trivial).toBe(0);
  });

  it('treats an equivalence or sameness with only one distinct member as trivial', () => {
    expect(trivialAxiom({ type: 'EquivalentClasses', classes: ['urn:A', 'urn:A'] })).toBe(true);
    expect(trivialAxiom({ type: 'EquivalentClasses', classes: ['urn:A', 'urn:B'] })).toBe(false);
    expect(trivialAxiom({ type: 'SameIndividual', individuals: ['urn:a'] })).toBe(true);
    expect(trivialAxiom({ type: 'SubClassOf', subClass: 'urn:A', superClass: OWL + 'Thing' })).toBe(
      true,
    );
    expect(
      trivialAxiom({
        type: 'SubClassOf',
        subClass: 'urn:A',
        superClass: { type: 'ObjectSomeValuesFrom' },
      }),
    ).toBe(false);
  });

  it('keeps an inferred annotation and declaration in their functional form', () => {
    const items = [
      item(
        {
          type: 'AnnotationAssertion',
          property: RDFS + 'comment',
          subject: 'urn:i',
          value: { value: 'derived comment' },
        },
        'AnnotationAssertion(rdfs:comment :i "derived comment")',
      ),
      item({ type: 'Declaration', entity: { type: 'NamedIndividual', iri: 'urn:i' } }, 'Decl'),
    ];
    const f = materializedClauses('NamedIndividual', 'urn:i', items, new Set(), name, {});
    expect(f.other.map((r) => r.ofn)).toEqual([
      'AnnotationAssertion(rdfs:comment :i "derived comment")',
      'Decl',
    ]);
    expect(f.trivial).toBe(0);
  });

  it('counts a top only in its own role', () => {
    expect(
      trivialAxiom({
        type: 'SubObjectPropertyOf',
        subProperty: 'urn:p',
        superProperty: OWL + 'Thing',
      }),
    ).toBe(false);
    expect(
      trivialAxiom({
        type: 'SubObjectPropertyOf',
        subProperty: 'urn:p',
        superProperty: OWL + 'topDataProperty',
      }),
    ).toBe(false);
    expect(
      trivialAxiom({
        type: 'SubDataPropertyOf',
        subProperty: 'urn:p',
        superProperty: OWL + 'topDataProperty',
      }),
    ).toBe(true);
    expect(
      trivialAxiom({
        type: 'ClassAssertion',
        class: OWL + 'topObjectProperty',
        individual: 'urn:i',
      }),
    ).toBe(false);
    expect(
      trivialAxiom({
        type: 'SubClassOf',
        subClass: 'urn:A',
        superClass: OWL + 'topObjectProperty',
      }),
    ).toBe(false);
  });

  it('writes inferred characteristics in their Manchester clause after the sections', () => {
    const items = [
      item({ type: 'TransitiveObjectProperty', property: 'urn:p' }, 'T'),
      item({ type: 'InverseFunctionalObjectProperty', property: 'urn:p' }, 'IF'),
      item({ type: 'ObjectPropertyDomain', property: 'urn:p', domain: 'urn:D' }, 'dom'),
    ];
    const f = materializedClauses('ObjectProperty', 'urn:p', items, new Set(), name, {});
    expect(f.clauses.map((c) => [c.word, c.rows.map((r) => text(r.tokens))])).toEqual([
      ['Domain', ['D']],
      ['Characteristics', ['Transitive', 'InverseFunctional']],
    ]);
    const data = materializedClauses(
      'DataProperty',
      'urn:d',
      [item({ type: 'FunctionalDataProperty', property: 'urn:d' }, 'F')],
      new Set(),
      name,
      {},
    );
    expect(data.clauses.map((c) => [c.word, c.rows.map((r) => text(r.tokens))])).toEqual([
      ['Characteristics', ['Functional']],
    ]);
  });

  it('leaves a characteristic in its functional form in a frame of another kind', () => {
    const items = [item({ type: 'TransitiveObjectProperty', property: 'urn:p' }, 'T')];
    const f = materializedClauses('NamedIndividual', 'urn:p', items, new Set(), name, {});
    expect(f.clauses).toEqual([]);
    expect(f.other.map((r) => r.ofn)).toEqual(['T']);
    const data = materializedClauses(
      'ObjectProperty',
      'urn:d',
      [item({ type: 'FunctionalDataProperty', property: 'urn:d' }, 'F')],
      new Set(),
      name,
      {},
    );
    expect(data.other.map((r) => r.ofn)).toEqual(['F']);
  });
});
