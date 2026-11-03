import { describe, expect, it } from 'vitest';
import type { Statement } from './browse';
import {
  annotationsOp,
  axiomOp,
  definitionProperty,
  findParentAxiom,
  inGraph,
  isAnnotation,
  parentAxiom,
  parentRows,
  sourcesOf,
  statementOp,
  textLit,
  toAnnotation,
} from './browse-edit';
import type { AxiomItem } from './ontology';

const GO = 'http://purl.obolibrary.org/obo/GO_0005739';
const OBO = 'http://purl.obolibrary.org/obo/';
const OIO = 'http://www.geneontology.org/formats/oboInOwl#';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const DEF = OBO + 'IAO_0000115';

const def: Statement = {
  property: DEF,
  value: { '@value': 'An organelle.', '@language': 'en' },
  graph: 'default',
  annotations: [
    { property: OIO + 'hasDbXref', value: { '@value': 'GOC:giardia' } },
    { property: RDFS + 'comment', value: { '@value': 'kept' } },
  ],
};

describe('statementOp on an OWL page', () => {
  it('replaces a definition and keeps its other annotations and known sources', () => {
    const op = statementOp(
      'owl',
      GO,
      def,
      {
        property: DEF,
        value: textLit('An organelle of cells.', 'en'),
        sources: ['GOC:giardia', 'ISBN:0198506732'],
      },
      null,
    );
    expect(op?.kind).toBe('axioms');
    if (op?.kind !== 'axioms') return;
    expect(op.remove).toEqual([
      {
        type: 'AnnotationAssertion',
        property: DEF,
        subject: GO,
        value: def.value,
        annotations: def.annotations,
      },
    ]);
    expect(op.add).toEqual([
      {
        type: 'AnnotationAssertion',
        property: DEF,
        subject: GO,
        value: { '@value': 'An organelle of cells.', '@language': 'en' },
        annotations: [
          { property: RDFS + 'comment', value: { '@value': 'kept' } },
          { property: OIO + 'hasDbXref', value: { '@value': 'GOC:giardia' } },
          { property: OIO + 'hasDbXref', value: { '@value': 'ISBN:0198506732' } },
        ],
      },
    ]);
  });

  it('answers null when the form changed nothing', () => {
    expect(
      statementOp(
        'owl',
        GO,
        def,
        { property: DEF, value: def.value, sources: ['GOC:giardia'] },
        null,
      ),
    ).toBeNull();
  });

  it('changes a synonym kind with one removal and one addition that keep the sources', () => {
    const syn: Statement = {
      property: OIO + 'hasExactSynonym',
      value: { '@value': 'mitochondria' },
      graph: 'default',
      annotations: [{ property: OIO + 'hasDbXref', value: { '@value': 'GOC:pr' } }],
    };
    const op = statementOp(
      'owl',
      GO,
      syn,
      { property: OIO + 'hasRelatedSynonym', value: syn.value, sources: sourcesOf(syn) },
      null,
    );
    if (op?.kind !== 'axioms') throw new Error('expected axioms');
    expect(op.remove).toHaveLength(1);
    expect(op.add[0]).toMatchObject({
      property: OIO + 'hasRelatedSynonym',
      annotations: [{ property: OIO + 'hasDbXref', value: { '@value': 'GOC:pr' } }],
    });
  });

  it('adds and removes a statement', () => {
    const add = statementOp(
      'owl',
      GO,
      null,
      { property: OIO + 'hasExactSynonym', value: textLit('mito', ''), sources: [] },
      null,
    );
    expect(add).toEqual({
      kind: 'axioms',
      add: [
        {
          type: 'AnnotationAssertion',
          property: OIO + 'hasExactSynonym',
          subject: GO,
          value: { '@value': 'mito' },
        },
      ],
      remove: [],
    });
    const rem = statementOp('owl', GO, def, null, null);
    expect(rem?.kind === 'axioms' && rem.add.length === 0 && rem.remove.length === 1).toBe(true);
  });
});

describe('statementOp on a SKOS page', () => {
  it('writes ground triples into the edited graph', () => {
    const c = 'http://example.org/thesaurus/fish';
    const op = statementOp(
      'skos',
      c,
      null,
      {
        property: definitionProperty('skos'),
        value: textLit('A cold-blooded animal.', 'EN'),
        sources: [],
      },
      'http://example.org/g',
    );
    expect(op).toEqual({
      kind: 'update',
      sparql: `INSERT DATA { GRAPH <http://example.org/g> { <${c}> <${SKOS}definition> "A cold-blooded animal."@en . } }`,
    });
  });

  it('answers null for an unchanged triple', () => {
    const s: Statement = {
      property: SKOS + 'definition',
      value: { '@value': 'x' },
      graph: 'default',
      annotations: [],
    };
    expect(statementOp('skos', 'urn:c', s, { ...s, sources: [] }, null)).toBeNull();
  });
});

describe('annotations', () => {
  const kinds = (i: string) => (i === 'urn:part_of' ? ['objectProperty'] : undefined);
  const owned = new Set([DEF]);
  it('leaves out axioms, facts, SKOS links and owned properties', () => {
    const is = (property: string) => isAnnotation({ property, value: 'x' }, owned, kinds);
    expect(is(RDFS + 'label')).toBe(true);
    expect(is(RDFS + 'comment')).toBe(true);
    expect(is(OIO + 'hasDbXref')).toBe(true);
    expect(is(SKOS + 'scopeNote')).toBe(true);
    expect(is(RDFS + 'subClassOf')).toBe(false);
    expect(is('http://www.w3.org/1999/02/22-rdf-syntax-ns#type')).toBe(false);
    expect(is('http://www.w3.org/2002/07/owl#equivalentClass')).toBe(false);
    expect(is('urn:part_of')).toBe(false);
    expect(is(SKOS + 'broader')).toBe(false);
    expect(is(SKOS + 'prefLabel')).toBe(false);
    expect(is(DEF)).toBe(false);
  });

  it('diffs the rows into an axioms edit on an OWL page', () => {
    const label: Statement = {
      property: RDFS + 'label',
      value: { '@value': 'mitochondrion', '@language': 'en' },
      graph: 'default',
      annotations: [],
    };
    const next = [toAnnotation(label), { property: RDFS + 'comment', value: { '@value': 'New.' } }];
    expect(annotationsOp('owl', GO, [label], next, null)).toEqual({
      kind: 'axioms',
      add: [
        {
          type: 'AnnotationAssertion',
          property: RDFS + 'comment',
          subject: GO,
          value: { '@value': 'New.' },
        },
      ],
      remove: [],
    });
    expect(annotationsOp('owl', GO, [label], [toAnnotation(label)], null)).toBeNull();
  });

  it('diffs the rows into triples on a SKOS page', () => {
    const note: Statement = {
      property: SKOS + 'scopeNote',
      value: { '@value': 'old' },
      graph: 'default',
      annotations: [],
    };
    const op = annotationsOp(
      'skos',
      'urn:c',
      [note],
      [{ property: SKOS + 'scopeNote', value: { '@value': 'new' } }],
      null,
    );
    expect(op).toEqual({
      kind: 'update',
      sparql: `DELETE DATA { <urn:c> <${SKOS}scopeNote> "old" . } ;\nINSERT DATA { <urn:c> <${SKOS}scopeNote> "new" . }`,
    });
  });
});

describe('parents', () => {
  const sub = (sup: string, graph = 'default'): AxiomItem => ({
    type: 'SubClassOf',
    ofn: '',
    axiom: { type: 'SubClassOf', subClass: GO, superClass: sup },
    graph,
  });
  it('builds the hierarchy axiom of each kind', () => {
    expect(parentAxiom('class', 'urn:a', 'urn:b')).toEqual({
      type: 'SubClassOf',
      subClass: 'urn:a',
      superClass: 'urn:b',
    });
    expect(parentAxiom('objectProperty', 'urn:a', 'urn:b')).toEqual({
      type: 'SubObjectPropertyOf',
      subProperty: 'urn:a',
      superProperty: 'urn:b',
    });
    expect(parentAxiom('concept', 'urn:a', 'urn:b')).toBeNull();
  });

  it('marks parents another graph states as not removable', () => {
    const axioms = [sub('urn:p1'), sub('urn:p2', 'urn:imported')];
    const rows = parentRows(
      'class',
      GO,
      [
        [GO, 'urn:p1'],
        [GO, 'urn:p2'],
        ['urn:p1', 'urn:root'],
        [GO, 'urn:p1'],
      ],
      axioms,
      null,
    );
    expect(rows.map((r) => [r.iri, !!r.axiom])).toEqual([
      ['urn:p1', true],
      ['urn:p2', false],
    ]);
    expect(findParentAxiom('class', GO, 'urn:p1', axioms)).toBe(axioms[0]);
    expect(findParentAxiom('class', GO, 'urn:none', axioms)).toBeNull();
  });

  it('adds or removes one axiom', () => {
    const a = parentAxiom('class', GO, 'urn:p')!;
    expect(axiomOp(a, false)).toEqual({ kind: 'axioms', add: [a], remove: [] });
    expect(axiomOp(a, true)).toEqual({ kind: 'axioms', add: [], remove: [a] });
  });
});

describe('graphs', () => {
  it('matches the default graph by name', () => {
    expect(inGraph({ graph: 'default' }, null)).toBe(true);
    expect(inGraph({}, null)).toBe(true);
    expect(inGraph({ graph: 'urn:g' }, null)).toBe(false);
    expect(inGraph({ graph: 'urn:g' }, 'urn:g')).toBe(true);
  });
});
