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
  withAssertions,
  withPending,
} from './browse-edit';
import type { AxiomItem, AxiomJson } from './ontology';

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

const sub = (sup: string): AxiomItem => ({
  type: 'SubClassOf',
  ofn: '',
  axiom: { type: 'SubClassOf', subClass: GO, superClass: sup },
  graph: 'default',
});

describe('the assertions the store keeps', () => {
  const DC = 'http://purl.org/dc/elements/1.1/';
  const xref = (v: string) => ({ property: OIO + 'hasDbXref', value: { '@value': v } });
  const value = { '@value': 'An organelle.', '@language': 'en' };
  const assertion = (annotations: AxiomJson['annotations']): AxiomItem => ({
    type: 'AnnotationAssertion',
    ofn: '',
    graph: 'default',
    axiom: { type: 'AnnotationAssertion', property: DEF, subject: GO, value, annotations },
  });
  // two reifications of one triple, which the term route merges into one statement
  const a1 = assertion([xref('GOC:a')]);
  const a2 = assertion([
    { ...xref('GOC:b'), annotations: [{ property: RDFS + 'comment', value: { '@value': 'n' } }] },
  ]);
  const merged: Statement = {
    property: DEF,
    value,
    graph: 'default',
    annotations: [xref('GOC:a'), xref('GOC:b')],
  };
  const [stated] = withAssertions(GO, [merged], [a1, a2, sub('urn:p')], null);

  it('removes every axiom that states the edited definition and keeps nested annotations', () => {
    expect(stated.asserted).toEqual([a1.axiom, a2.axiom]);
    const op = statementOp(
      'owl',
      GO,
      stated,
      {
        property: DEF,
        value: textLit('An organelle of cells.', 'en'),
        sources: ['GOC:a', 'GOC:b'],
      },
      null,
    );
    if (op?.kind !== 'axioms') throw new Error('expected axioms');
    expect(op.remove).toEqual([a1.axiom, a2.axiom]);
    expect(op.add).toHaveLength(1);
    expect((op.add[0] as AxiomJson).annotations).toEqual([
      a1.axiom.annotations![0],
      a2.axiom.annotations![0],
    ]);
    const gone = statementOp('owl', GO, stated, null, null);
    expect(gone?.kind === 'axioms' && gone.remove).toEqual([a1.axiom, a2.axiom]);
  });

  it('leaves an unchanged annotation row with several axioms alone', () => {
    expect(annotationsOp('owl', GO, [stated], [toAnnotation(stated)], null)).toBeNull();
    const op = annotationsOp('owl', GO, [stated], [], null);
    expect(op?.kind === 'axioms' && op.remove).toEqual([a1.axiom, a2.axiom]);
  });

  it('keeps sources of different properties with the same text', () => {
    const s: Statement = {
      ...merged,
      annotations: [xref('X'), { property: DC + 'source', value: { '@value': 'X' } }],
    };
    expect(sourcesOf(s)).toEqual(['X']);
    const op = statementOp(
      'owl',
      GO,
      s,
      { property: DEF, value: textLit('New.', 'en'), sources: ['X'] },
      null,
    );
    if (op?.kind !== 'axioms') throw new Error('expected axioms');
    expect((op.add[0] as AxiomJson).annotations).toEqual(s.annotations);
  });

  it('changes an annotated SKOS definition as an assertion, so its sources follow', () => {
    const c = 'http://example.org/thesaurus/fish';
    const d: Statement = {
      property: SKOS + 'definition',
      value: { '@value': 'old' },
      graph: 'default',
      annotations: [xref('ISBN:1')],
    };
    // the assertion the term route answers for it, declared as an annotation property
    const stated = {
      ...d,
      asserted: [
        {
          type: 'AnnotationAssertion',
          property: SKOS + 'definition',
          subject: c,
          value: d.value,
          annotations: [xref('ISBN:1')],
        },
      ],
    };
    const op = statementOp(
      'skos',
      c,
      stated,
      { property: SKOS + 'definition', value: textLit('new', ''), sources: sourcesOf(d) },
      null,
    );
    if (op?.kind !== 'axioms') throw new Error('expected axioms');
    expect(op.add[0]).toMatchObject({ value: { '@value': 'new' }, annotations: [xref('ISBN:1')] });
  });

  it('counts a source typed and not added', () => {
    expect(withPending(['A'], ' B ')).toEqual(['A', 'B']);
    expect(withPending(['A'], 'A')).toEqual(['A']);
    expect(withPending(['A'], '')).toEqual(['A']);
  });
});

describe('SKOS triples the mapper reads as data property assertions', () => {
  const c = 'http://example.org/thesaurus/fish';
  const xref = { property: OIO + 'hasDbXref', value: { '@value': 'ISBN:1' } };
  const d: Statement = {
    property: SKOS + 'definition',
    value: { '@value': 'old' },
    graph: 'default',
    annotations: [xref],
  };
  const stored: AxiomItem = {
    type: 'DataPropertyAssertion',
    ofn: '',
    graph: 'default',
    axiom: {
      type: 'DataPropertyAssertion',
      property: SKOS + 'definition',
      source: c,
      value: { '@value': 'old' },
      annotations: [xref],
    },
  };

  it('removes the stored assertion and adds one of the same kind', () => {
    const [s] = withAssertions(c, [d], [stored], null);
    expect(s.asserted).toEqual([stored.axiom]);
    const op = statementOp(
      'skos',
      c,
      s,
      { property: SKOS + 'definition', value: textLit('new', ''), sources: sourcesOf(s) },
      null,
    );
    expect(op).toEqual({
      kind: 'axioms',
      remove: [stored.axiom],
      add: [
        {
          type: 'DataPropertyAssertion',
          property: SKOS + 'definition',
          source: c,
          value: { '@value': 'new' },
          annotations: [xref],
        },
      ],
    });
  });

  it('retargets the reifications when no assertion was answered', () => {
    const op = statementOp(
      'skos',
      c,
      d,
      { property: SKOS + 'definition', value: textLit('new', ''), sources: [] },
      'urn:g',
    );
    if (op?.kind !== 'update') throw new Error('expected an update');
    expect(op.sparql).toContain(
      `DELETE DATA { GRAPH <urn:g> { <${c}> <${SKOS}definition> "old" . } }`,
    );
    expect(op.sparql).toContain(
      `INSERT DATA { GRAPH <urn:g> { <${c}> <${SKOS}definition> "new" . } }`,
    );
    expect(op.sparql).toContain('owl#annotatedTarget> "new"');
  });
});

describe('a draft whose statement is gone', () => {
  it('adds the draft with the property and annotations of the statement it began from', () => {
    const DC = 'http://purl.org/dc/elements/1.1/';
    const gone: Statement = {
      property: SKOS + 'definition',
      value: { '@value': 'old' },
      graph: 'default',
      annotations: [
        { property: DC + 'source', value: 'http://example.org/book' },
        { property: RDFS + 'comment', value: { '@value': 'kept' } },
      ],
    };
    for (const mode of ['owl', 'skos'] as const) {
      const op = statementOp(
        mode,
        GO,
        null,
        { property: gone.property, value: textLit('mine', ''), sources: sourcesOf(gone) },
        null,
        gone,
      );
      if (op?.kind !== 'axioms') throw new Error('expected axioms');
      expect(op.remove).toEqual([]);
      expect(op.add).toEqual([
        {
          type: 'AnnotationAssertion',
          property: SKOS + 'definition',
          subject: GO,
          value: { '@value': 'mine' },
          annotations: [gone.annotations[1], gone.annotations[0]],
        },
      ]);
    }
  });
});
