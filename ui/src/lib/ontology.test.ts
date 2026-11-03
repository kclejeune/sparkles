import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ancestorsOf,
  buildFrame,
  buildTree,
  dataUpdate,
  defaultNamespace,
  editableIri,
  filterTree,
  hierarchyEdges,
  labelsOf,
  labelsOfBindings,
  labelsQuery,
  withMissingLabels,
  localFromLabel,
  manchesterTokens,
  litText,
  manchester,
  manchesterName,
  mintIri,
  pickLabel,
  renameUpdate,
  resolveName,
  revertCommit,
  replaceAnnotation,
  sectionAxiom,
  sessionBranchName,
  taxonomyTree,
  UndoStack,
  validLang,
  withAnnotations,
  type Annotation,
  type AxiomItem,
  type Classification,
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

describe('Manchester tokens', () => {
  const name = (i: string) =>
    i === EX + 'pc' ? 'part of' : i === EX + 'and' ? 'and' : i.replace(EX, '');
  const join = (e: unknown) =>
    manchesterTokens(e, name, {})
      .map((t) => t.text)
      .join('');
  const samples: unknown[] = [
    EX + 'A',
    { type: 'ObjectSomeValuesFrom', property: EX + 'pc', class: EX + 'Cell' },
    {
      type: 'ObjectIntersectionOf',
      classes: [
        EX + 'and',
        {
          type: 'ObjectAllValuesFrom',
          property: EX + 'p',
          class: { type: 'ObjectComplementOf', class: EX + 'B' },
        },
      ],
    },
    { type: 'ObjectUnionOf', classes: [EX + 'A', EX + 'B', EX + 'C'] },
    { type: 'ObjectOneOf', individuals: [EX + 'a', EX + 'b'] },
    { type: 'ObjectHasValue', property: { inverseOf: EX + 'p' }, individual: EX + 'a' },
    { type: 'ObjectHasSelf', property: EX + 'p' },
    { type: 'ObjectExactCardinality', property: EX + 'p', cardinality: 1, class: EX + 'A' },
    {
      type: 'DataSomeValuesFrom',
      property: EX + 'd',
      range: 'http://www.w3.org/2001/XMLSchema#string',
    },
    { '@value': "it's", '@language': 'en' },
    { '@value': '3', '@type': 'http://www.w3.org/2001/XMLSchema#integer' },
    { type: 'Chain', chain: [EX + 'p', EX + 'q'] },
  ];
  it('join to the text of manchester()', () => {
    for (const e of samples) expect(join(e)).toBe(manchester(e, name, {}));
  });
  it('mark names with their IRIs, keywords and literals', () => {
    const t = manchesterTokens(samples[1], name, {});
    expect(t.filter((x) => x.role === 'name').map((x) => [x.text, x.iri])).toEqual([
      ["'part of'", EX + 'pc'],
      ['Cell', EX + 'Cell'],
    ]);
    expect(t.filter((x) => x.role === 'kw').map((x) => x.text)).toEqual(['some']);
    const lit = manchesterTokens(samples[9], name, {});
    expect(lit.map((x) => x.role)).toEqual(['lit']);
  });
  it('quote a label that is a keyword and parenthesize nested expressions', () => {
    const t = manchesterTokens(samples[2], name, {});
    expect(t.find((x) => x.iri === EX + 'and')?.text).toBe("'and'");
    expect(t.some((x) => x.role === 'punct' && x.text === '(')).toBe(true);
    expect(join(samples[2])).toBe("'and' and (p only not B)");
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
  it('reverts the commits in order and clears redo on a new edit', () => {
    const s = new UndoStack();
    s.push({ label: 'one', commit: 3 });
    s.push({ label: 'two', commit: 4 });
    // commit 5 reverts commit 4, so a redo reverts commit 5
    s.undid(5);
    expect(s.done).toEqual([{ label: 'one', commit: 3 }]);
    expect(s.undone).toEqual([{ label: 'two', commit: 5 }]);
    // commit 6 reverts the revert, so an undo reverts commit 6
    s.redid(6);
    expect(s.done).toEqual([
      { label: 'one', commit: 3 },
      { label: 'two', commit: 6 },
    ]);
    expect(s.undone).toEqual([]);
    s.undid(7);
    s.push({ label: 'three', commit: 8 });
    expect(s.undone).toEqual([]);
  });

  it('drops an entry when its revert made no commit', () => {
    const s = new UndoStack();
    s.push({ label: 'one', commit: 3 });
    s.undid(null);
    expect(s.done).toEqual([]);
    expect(s.undone).toEqual([]);
  });

  afterEach(() => void vi.unstubAllGlobals());

  it('drops stored entries without a commit', () => {
    const store = new Map<string, string>();
    vi.stubGlobal('sessionStorage', {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    });
    sessionStorage.setItem(
      'undo-test',
      JSON.stringify({
        done: [
          { label: 'old', commit: 1, inverse: {} },
          { label: 'older', inverse: {} },
        ],
        undone: [{ label: 'x', commit: null }],
      }),
    );
    const s = new UndoStack('undo-test');
    expect(s.done.map((d) => d.label)).toEqual(['old']);
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

describe('annotation drafts', () => {
  const label = (v: string, lang = 'en'): Annotation => ({
    property: RDFS + 'label',
    value: { '@value': v, '@language': lang },
  });

  it('replaces the edited row by its value after rows were added before it', () => {
    const original = label('Person');
    const refreshed = [label('Personne', 'fr'), label('Human'), original];
    expect(replaceAnnotation(refreshed, original, label('Person, a human'))).toEqual([
      label('Personne', 'fr'),
      label('Human'),
      label('Person, a human'),
    ]);
  });

  it('keeps the annotations of the replaced row', () => {
    const note: Annotation = { property: RDFS + 'comment', value: { '@value': 'checked' } };
    const original = { ...label('Person'), annotations: [note] };
    expect(replaceAnnotation([original], label('Person'), label('Persons'))).toBeNull();
    expect(replaceAnnotation([original], original, label('Persons'))).toEqual([
      { ...label('Persons'), annotations: [note] },
    ]);
  });

  it('refuses when the row is gone', () => {
    expect(replaceAnnotation([label('Human')], label('Person'), label('Persons'))).toBeNull();
  });

  it('holds IRIs as prefixed names or in angle brackets, never as labels', () => {
    expect(editableIri(EX + 'Person', { ex: EX })).toBe('ex:Person');
    expect(editableIri('http://other.org/x', { ex: EX })).toBe('<http://other.org/x>');
  });
});

describe('reverts', () => {
  afterEach(() => void vi.unstubAllGlobals());
  const answer = (body: unknown) => {
    const calls: { url: string; body: unknown }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, body: JSON.parse(String(init.body ?? '{}')) });
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    return calls;
  };

  it('previews the revert of a commit on the branch the target names', async () => {
    const calls = answer({ dryRun: true, wouldCommit: true, outcome: 'commit', head: 9 });
    const r = await revertCommit('uni@onto.ada.20261102', 4, { dryRun: true, head: 9 });
    expect(r).toMatchObject({ dryRun: true, outcome: 'commit' });
    expect(calls).toEqual([
      {
        url: '/$/revert/uni?branch=onto.ada.20261102&commit=4',
        body: { dryRun: true, changes: 50, expect: { target: 9 } },
      },
    ]);
  });

  it('reads an up-to-date answer to a dry run as a preview that changes nothing', async () => {
    answer({ upToDate: true, merged: false, commit: null, target: { branch: 'main', seq: 9 } });
    expect(await revertCommit('uni', 4, { dryRun: true, head: 9 })).toEqual({
      dryRun: true,
      wouldCommit: false,
      outcome: 'no-change',
      head: 9,
    });
  });

  it('answers the commit a revert made as its receipt', async () => {
    answer({ upToDate: false, commit: { seq: 10 }, changes: { inserted: 2, deleted: 3 } });
    const r = await revertCommit('uni', 4, { head: 9 });
    expect(r).toMatchObject({ inserted: 2, deleted: 3, receipt: { committed: true } });
    expect((r as { receipt: { commit: { seq: number } } }).receipt.commit.seq).toBe(10);
  });
});

describe('Manchester names and literals', () => {
  it('quotes a name the reader would not read back as one word', () => {
    expect(manchesterName('Person')).toBe('Person');
    expect(manchesterName('uni:Person')).toBe('uni:Person');
    expect(manchesterName('<http://ex.org/a b>')).toBe("'<http://ex.org/a b>'");
    expect(manchesterName('<http://ex.org/A>')).toBe('<http://ex.org/A>');
    expect(manchesterName('Graduate student')).toBe("'Graduate student'");
    expect(manchesterName('some')).toBe("'some'");
    expect(manchesterName('42nd street')).toBe("'42nd street'");
    expect(manchesterName("O'Brien")).toBe("'O\\'Brien'");
    expect(manchesterName('_:b0')).toBe("'_:b0'");
  });

  it('writes integers, decimals and booleans bare and other literals typed', () => {
    const xsd = 'http://www.w3.org/2001/XMLSchema#';
    const p = { xsd };
    expect(litText({ '@value': '6', '@type': xsd + 'integer' }, p)).toBe('6');
    expect(litText({ '@value': '-1.5', '@type': xsd + 'decimal' }, p)).toBe('-1.5');
    expect(litText({ '@value': 'true', '@type': xsd + 'boolean' }, p)).toBe('true');
    expect(litText({ '@value': '1', '@type': xsd + 'boolean' }, p)).toBe('"1"^^xsd:boolean');
    expect(litText({ '@value': '1e3', '@type': xsd + 'double' }, p)).toBe('"1e3"^^xsd:double');
    expect(litText({ '@value': 'chat', '@language': 'fr' }, p)).toBe('"chat"@fr');
  });

  it('quotes labels inside a class expression', () => {
    const label: Record<string, string> = {
      'http://ex.org/enrolledIn': 'enrolled in',
      'http://ex.org/GraduateCourse': 'Graduate course',
    };
    expect(
      manchester(
        {
          type: 'ObjectSomeValuesFrom',
          property: 'http://ex.org/enrolledIn',
          class: 'http://ex.org/GraduateCourse',
        },
        (i) => label[i] ?? i,
        {},
      ),
    ).toBe("'enrolled in' some 'Graduate course'");
  });
});

describe('names in the add fields', () => {
  const opts = [{ iri: 'http://ex.org/G', name: 'Graduate student' }];
  const prefixes = { ex: 'http://ex.org/' };
  it('resolves one name or IRI and leaves expressions to the reader', () => {
    expect(resolveName('Graduate student', opts, prefixes)).toBe('http://ex.org/G');
    expect(resolveName('ex:A', opts, prefixes)).toBe('http://ex.org/A');
    expect(resolveName(' <http://ex.org/B> ', opts, prefixes)).toBe('http://ex.org/B');
    expect(resolveName('ex:p some ex:A', opts, prefixes)).toBeNull();
    expect(resolveName('not ex:A', opts, prefixes)).toBeNull();
    expect(resolveName('{ex:a}', opts, prefixes)).toBeNull();
  });
});

describe('the labels a capped listing leaves out', () => {
  it('are read from the selected graph in one query', () => {
    const q = labelsQuery({ graph: null, imports: true });
    expect(q).toContain('<http://www.w3.org/2000/01/rdf-schema#label>');
    expect(q).toContain('<http://www.w3.org/2004/02/skos/core#prefLabel>');
    // the stored default graph, not the union a server may read as its default
    expect(q).toContain('GRAPH <urn:x-arq:DefaultGraph> {');
    expect(labelsQuery({ graph: 'http://ex.org/g', imports: true })).toContain(
      'GRAPH <http://ex.org/g> {',
    );
  });

  it('put rdfs:label before skos:prefLabel and keep languages', () => {
    const m = labelsOfBindings([
      {
        s: { type: 'uri', value: 'http://ex.org/a' },
        p: { type: 'uri', value: 'http://www.w3.org/2004/02/skos/core#prefLabel' },
        l: { type: 'literal', value: 'pref', 'xml:lang': 'en' },
      },
      {
        s: { type: 'uri', value: 'http://ex.org/a' },
        p: { type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#label' },
        l: { type: 'literal', value: 'label' },
      },
    ]);
    expect(m.get('http://ex.org/a')).toEqual([
      { '@value': 'label' },
      { '@value': 'pref', '@language': 'en' },
    ]);
  });

  it('fill only the subjects without a label', () => {
    const have = new Map([['a', [{ '@value': 'A' }]]]);
    const more = new Map([
      ['a', [{ '@value': 'other' }]],
      ['b', [{ '@value': 'B' }]],
    ]);
    const out = withMissingLabels(have, more);
    expect(out.get('a')).toEqual([{ '@value': 'A' }]);
    expect(out.get('b')).toEqual([{ '@value': 'B' }]);
    expect(have.has('b')).toBe(false);
  });
});
