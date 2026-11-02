import { describe, expect, it } from 'vitest';
import {
  ancestorsOf,
  buildFrame,
  buildTree,
  dataUpdate,
  defaultNamespace,
  filterTree,
  hierarchyEdges,
  inverseOfEdit,
  labelsOf,
  localFromLabel,
  manchester,
  mintIri,
  pickLabel,
  renameUpdate,
  sectionAxiom,
  sessionBranchName,
  taxonomyTree,
  UndoStack,
  validLang,
  withAnnotations,
  type AxiomItem,
  type Classification,
  type EditAnswer,
  type IriPolicy,
} from './ontology';

const EX = 'http://ex.org/';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const item = (axiom: AxiomItem['axiom']): AxiomItem => ({ type: axiom.type, ofn: '', axiom });

describe('trees', () => {
  it('roots the nodes without a parent among them', () => {
    const t = buildTree(
      [EX + 'Person', EX + 'Student', EX + 'Course'],
      [
        [EX + 'Student', EX + 'Person'],
        [EX + 'Person', 'http://www.w3.org/2002/07/owl#Thing'],
        [EX + 'Course', EX + 'Elsewhere'],
      ],
    );
    expect(t.roots).toEqual([EX + 'Course', EX + 'Person']);
    expect(t.children.get(EX + 'Person')).toEqual([EX + 'Student']);
  });

  it('keeps several parents and reaches a cycle', () => {
    const t = buildTree(
      ['a', 'b', 'c', 'd'],
      [
        ['c', 'a'],
        ['c', 'b'],
        ['a', 'd'],
        ['d', 'a'],
      ],
    );
    // a and d only reach each other: the first of them roots the cycle
    expect(t.roots).toEqual(['b', 'a']);
    expect(t.parents.get('c')).toEqual(['a', 'b']);
    expect(ancestorsOf(t, 'c').sort()).toEqual(['a', 'b', 'd']);
  });

  it('filters to the matches and their ancestors', () => {
    const t = buildTree(['a', 'b', 'c'], [['c', 'b']]);
    const keep = filterTree(t, (n) => ({ a: 'Alpha', b: 'Beta', c: 'Gamma' })[n]!, 'gam');
    expect([...keep!].sort()).toEqual(['b', 'c']);
    expect(filterTree(t, (n) => n, '  ')).toBeNull();
  });

  it('reads the named hierarchy of each kind', () => {
    const ax = [
      item({ type: 'SubClassOf', subClass: EX + 'A', superClass: EX + 'B' }),
      item({
        type: 'SubClassOf',
        subClass: EX + 'A',
        superClass: { type: 'ObjectSomeValuesFrom' },
      }),
      item({ type: 'SubObjectPropertyOf', subProperty: EX + 'p', superProperty: EX + 'q' }),
      item({ type: 'SubObjectPropertyOf', chain: [EX + 'p', EX + 'q'], superProperty: EX + 'r' }),
      item({ type: 'ClassAssertion', class: EX + 'A', individual: EX + 'i' }),
    ];
    expect(hierarchyEdges('Class', ax)).toEqual([[EX + 'A', EX + 'B']]);
    expect(hierarchyEdges('ObjectProperty', ax)).toEqual([[EX + 'p', EX + 'q']]);
    expect(hierarchyEdges('NamedIndividual', ax)).toEqual([[EX + 'i', EX + 'A']]);
  });

  it('builds the inferred tree from the taxonomy nodes', () => {
    const c = {
      nodes: [
        {
          node: 0,
          classes: ['http://www.w3.org/2002/07/owl#Thing'],
          parents: [],
          parentClasses: [],
          children: 1,
        },
        {
          node: 1,
          classes: ['http://www.w3.org/2002/07/owl#Nothing', EX + 'Bad'],
          parents: [3],
          parentClasses: [],
          children: 0,
        },
        {
          node: 2,
          classes: [EX + 'Person', EX + 'Human'],
          parents: [0],
          parentClasses: [],
          children: 1,
        },
        { node: 3, classes: [EX + 'Student'], parents: [2], parentClasses: [], children: 1 },
      ],
    } as unknown as Classification;
    const { tree, classesOf, nodeOf } = taxonomyTree(c);
    expect(tree.roots).toEqual(['n:2']);
    expect(classesOf.get('n:2')).toEqual([EX + 'Person', EX + 'Human']);
    expect(nodeOf.get(EX + 'Human')).toBe('n:2');
    expect(tree.children.get('n:3')).toEqual(['n:1']);
  });
});

describe('labels', () => {
  const lits = [
    { '@value': 'Person', '@language': 'en' },
    { '@value': 'Personne', '@language': 'fr' },
    { '@value': 'Persona', '@language': 'es-MX' },
  ];
  it('prefers the asked languages, then a primary subtag, then English', () => {
    expect(pickLabel(lits, ['fr'])).toBe('Personne');
    expect(pickLabel(lits, ['es'])).toBe('Persona');
    expect(pickLabel(lits, ['de'])).toBe('Person');
    expect(pickLabel([{ '@value': 'x', '@language': 'de' }, { '@value': 'plain' }], ['fr'])).toBe(
      'plain',
    );
    expect(pickLabel(undefined)).toBeNull();
  });

  it('reads rdfs:label before skos:prefLabel', () => {
    const m = labelsOf([
      item({
        type: 'AnnotationAssertion',
        property: 'http://www.w3.org/2004/02/skos/core#prefLabel',
        subject: EX + 'A',
        value: { '@value': 'pref' },
      }),
      item({
        type: 'AnnotationAssertion',
        property: RDFS + 'label',
        subject: EX + 'A',
        value: { '@value': 'label' },
      }),
      item({
        type: 'AnnotationAssertion',
        property: RDFS + 'comment',
        subject: EX + 'A',
        value: { '@value': 'no' },
      }),
    ]);
    expect(m.get(EX + 'A')!.map((l) => l['@value'])).toEqual(['label', 'pref']);
  });
});

describe('Manchester display', () => {
  const name = (i: string) => i.replace(EX, '');
  it('writes restrictions, connectives and literals', () => {
    expect(
      manchester(
        {
          type: 'ObjectIntersectionOf',
          classes: [
            EX + 'Person',
            { type: 'ObjectSomeValuesFrom', property: EX + 'teaches', class: EX + 'Course' },
          ],
        },
        name,
        {},
      ),
    ).toBe('Person and (teaches some Course)');
    expect(
      manchester(
        {
          type: 'ObjectMinCardinality',
          property: { inverseOf: EX + 'p' },
          cardinality: 2,
          class: EX + 'A',
        },
        name,
        {},
      ),
    ).toBe('inverse p min 2 A');
    expect(manchester({ '@value': 'chat', '@language': 'fr' }, name, {})).toBe('"chat"@fr');
    expect(
      manchester(
        {
          type: 'DatatypeRestriction',
          datatype: 'http://www.w3.org/2001/XMLSchema#integer',
          restrictions: [
            {
              facet: 'http://www.w3.org/2001/XMLSchema#minInclusive',
              value: { '@value': '3', '@type': 'http://www.w3.org/2001/XMLSchema#integer' },
            },
          ],
        },
        (i) => (i.endsWith('integer') ? 'xsd:integer' : i),
        {},
      ),
    ).toBe('xsd:integer[>= 3]');
  });
});

describe('frames', () => {
  it('sorts the axioms about a class into its sections', () => {
    const A = EX + 'A';
    const f = buildFrame('Class', A, [
      item({ type: 'Declaration', kind: 'Class', iri: A }),
      item({
        type: 'AnnotationAssertion',
        property: RDFS + 'label',
        subject: A,
        value: { '@value': 'A' },
      }),
      item({ type: 'SubClassOf', subClass: A, superClass: EX + 'B' }),
      item({ type: 'SubClassOf', subClass: EX + 'C', superClass: A }),
      item({ type: 'EquivalentClasses', classes: [A, EX + 'D'] }),
      item({ type: 'ClassAssertion', class: A, individual: EX + 'i' }),
      item({ type: 'ObjectPropertyDomain', property: EX + 'p', domain: A }),
    ]);
    const rows = (id: string) => f.sections.find((s) => s.id === id)!.rows.map((r) => r.shown);
    expect(rows('sub')).toEqual([[EX + 'B']]);
    expect(rows('superOf')).toEqual([[EX + 'C']]);
    expect(rows('equivalent')).toEqual([[EX + 'D']]);
    expect(rows('instances')).toEqual([[EX + 'i']]);
    expect(f.annotations).toHaveLength(1);
    expect(f.declarations).toHaveLength(1);
    expect(f.other.map((o) => o.type)).toEqual(['ObjectPropertyDomain']);
  });

  it('builds the axiom of an added row, named or as a functional-style expression', () => {
    const A = EX + 'A';
    expect(sectionAxiom('Class', 'sub', A, EX + 'B')).toEqual({
      type: 'SubClassOf',
      subClass: A,
      superClass: EX + 'B',
    });
    expect(sectionAxiom('Class', 'sub', A, 'ObjectSomeValuesFrom(:p :C)')).toBe(
      `SubClassOf(<${A}> ObjectSomeValuesFrom(:p :C))`,
    );
    expect(sectionAxiom('Class', 'superOf', A, 'ObjectUnionOf(:B :C)')).toBe(
      `SubClassOf(ObjectUnionOf(:B :C) <${A}>)`,
    );
    expect(sectionAxiom('NamedIndividual', 'types', EX + 'i', EX + 'A')).toEqual({
      type: 'ClassAssertion',
      class: EX + 'A',
      individual: EX + 'i',
    });
    expect(sectionAxiom('ObjectProperty', 'inverses', EX + 'p', 'ObjectInverseOf(:q)')).toBeNull();
  });

  it('replaces an axiom’s annotations', () => {
    const a = {
      type: 'SubClassOf',
      subClass: 'a',
      superClass: 'b',
      annotations: [{ property: 'p', value: 'x' }],
    };
    expect(withAnnotations(a, [])).toEqual({ type: 'SubClassOf', subClass: 'a', superClass: 'b' });
    expect(withAnnotations(a, [{ property: 'q', value: 'y' }]).annotations).toEqual([
      { property: 'q', value: 'y' },
    ]);
  });
});

describe('undo', () => {
  const answer = (over: Partial<EditAnswer>): EditAnswer => ({
    added: 0,
    present: 0,
    removed: 0,
    absent: [],
    declared: [],
    addedAxioms: [],
    removedAxioms: [],
    axiomsTruncated: false,
    inserted: 0,
    deleted: 0,
    kept: 0,
    committed: true,
    ...over,
  });

  it('inverts what an edit wrote and removed', () => {
    const sub = { type: 'SubClassOf', subClass: 'a', superClass: 'b' };
    const decl = { type: 'Declaration', kind: 'Class', iri: 'a' };
    const old = { type: 'SubClassOf', subClass: 'a', superClass: 'c' };
    expect(
      inverseOfEdit(answer({ addedAxioms: [item(sub), item(decl)], removedAxioms: [item(old)] })),
    ).toEqual({
      kind: 'axioms',
      add: [old],
      remove: [sub, decl],
      declare: false,
    });
    expect(inverseOfEdit(answer({}))).toBeNull();
    expect(inverseOfEdit(answer({ addedAxioms: [item(sub)], axiomsTruncated: true }))).toBeNull();
  });

  it('keeps the stacks in order and clears redo on a new edit', () => {
    const s = new UndoStack();
    const op = (n: string) => ({
      label: n,
      commit: 1,
      inverse: { kind: 'update' as const, sparql: n, inverse: null },
    });
    s.push(op('one'));
    s.push(op('two'));
    s.undid(op('redo two'));
    expect(s.done.map((d) => d.label)).toEqual(['one']);
    expect(s.undone.map((d) => d.label)).toEqual(['redo two']);
    s.redid(op('undo two'));
    expect(s.done.map((d) => d.label)).toEqual(['one', 'undo two']);
    s.undid(op('redo again'));
    s.push(op('three'));
    expect(s.undone).toEqual([]);
  });
});

describe('IRIs', () => {
  const policy = (over: Partial<IriPolicy>): IriPolicy => ({
    namespace: EX,
    mode: 'label',
    prefix: 'EX_',
    digits: 5,
    ...over,
  });

  it('takes the namespace from the ontology IRI', () => {
    expect(defaultNamespace('http://ex.org/onto')).toBe('http://ex.org/onto#');
    expect(defaultNamespace('http://ex.org/onto/')).toBe('http://ex.org/onto/');
    expect(defaultNamespace(null)).toBe('http://example.org/ontology#');
  });

  it('mints a local name from the label by kind', () => {
    expect(localFromLabel('graduate student', 'Class')).toBe('GraduateStudent');
    expect(localFromLabel('Has advisor', 'ObjectProperty')).toBe('hasAdvisor');
    expect(localFromLabel('élève 2', 'Class')).toBe('Élève2');
    expect(localFromLabel('3D model', 'Class')).toBe('_3DModel');
    expect(localFromLabel('  ', 'Class')).toBe('');
  });

  it('avoids IRIs in use', () => {
    const taken = new Set([EX + 'Student', EX + 'Student2', EX + 'EX_00041', EX + 'EX_7']);
    expect(mintIri(policy({}), 'student', 'Class', taken)).toBe(EX + 'Student3');
    expect(mintIri(policy({ mode: 'sequence' }), '', 'Class', taken)).toBe(EX + 'EX_00042');
    const ids = ['1', '2'];
    expect(
      mintIri(policy({ mode: 'uuid' }), '', 'Class', new Set([EX + '1']), () => ids.shift()!),
    ).toBe(EX + '2');
  });

  it('checks language tags', () => {
    expect(validLang('en')).toBe(true);
    expect(validLang('zh-Hant-TW')).toBe(true);
    expect(validLang('')).toBe(true);
    expect(validLang('en_US')).toBe(false);
  });

  it('names the session branch', () => {
    expect(sessionBranchName('ana b', new Date('2026-11-02T10:00:00Z'))).toBe(
      'onto.ana-b.20261102',
    );
    expect(sessionBranchName(null, new Date('2026-11-02T10:00:00Z'))).toBe(
      'onto.anonymous.20261102',
    );
  });
});

describe('SPARQL', () => {
  it('renames in every position with sameTerm and a guard on the new IRI', () => {
    const u = renameUpdate(EX + 'Old', EX + 'New', EX + 'g');
    expect(u).toContain(`GRAPH <${EX}g> { ?s ?p ?o }`);
    expect(u).toContain(`BIND(IF(sameTerm(?o, <${EX}Old>), <${EX}New>, ?o) AS ?o2)`);
    expect(u).toContain(`FILTER NOT EXISTS { GRAPH <${EX}g> { { <${EX}New> ?a ?b }`);
    expect(renameUpdate(EX + 'a', EX + 'b', null)).not.toContain('GRAPH');
  });

  it('writes header edits with their inverse', () => {
    const t = { s: EX + 'o', p: RDFS + 'label', o: { '@value': 'Say "hi"', '@language': 'en' } };
    const v = {
      s: EX + 'o',
      p: 'http://www.w3.org/2002/07/owl#versionIRI',
      o: { iri: EX + 'o/2' },
    };
    const { sparql, inverse } = dataUpdate([t], [v], null);
    expect(sparql).toBe(
      `DELETE DATA { <${EX}o> <${RDFS}label> "Say \\"hi\\""@en . } ;\nINSERT DATA { <${EX}o> <http://www.w3.org/2002/07/owl#versionIRI> <${EX}o/2> . }`,
    );
    expect(
      inverse.startsWith(
        'DELETE DATA { <http://ex.org/o> <http://www.w3.org/2002/07/owl#versionIRI>',
      ),
    ).toBe(true);
  });
});
