import { describe, expect, it } from 'vitest';
import ox from 'oxigraph';
import {
  BULK_LIMIT,
  changesOp,
  compileFind,
  descendants,
  kindsOfTypes,
  typesQuery,
  findMatches,
  langMatches,
  namedParents,
  parseEntityList,
  planMove,
  removeAnnotation,
  setAnnotation,
  valueOf,
  type FindSpec,
} from './bulk-edit';
import type { Annotation, AxiomJson } from './ontology';

const EX = 'http://example.org/uni#';
const LABEL = 'http://www.w3.org/2000/01/rdf-schema#label';
const COMMENT = 'http://www.w3.org/2000/01/rdf-schema#comment';
const ALT = 'http://www.w3.org/2004/02/skos/core#altLabel';
const XSD = 'http://www.w3.org/2001/XMLSchema#';

const ann = (s: string, p: string, value: unknown, annotations?: Annotation[]): AxiomJson => ({
  type: 'AnnotationAssertion',
  property: p,
  subject: EX + s,
  value,
  ...(annotations ? { annotations } : {}),
});
const sub = (c: string, p: string | object, annotations?: Annotation[]): AxiomJson => ({
  type: 'SubClassOf',
  subClass: EX + c,
  superClass: typeof p === 'string' ? EX + p : p,
  ...(annotations ? { annotations } : {}),
});

const spec = (o: Partial<FindSpec>): FindSpec => ({
  find: '',
  replace: '',
  regex: false,
  caseSensitive: false,
  properties: { kind: 'labels' },
  lang: { kind: 'any' },
  entities: null,
  ...o,
});

function run(axioms: AxiomJson[], o: Partial<FindSpec>) {
  const s = spec(o);
  const re = compileFind(s);
  if (typeof re === 'string') throw new Error(re);
  return findMatches(axioms, s, re);
}

const AXIOMS = [
  ann('Student', LABEL, { '@value': 'Student', '@language': 'en' }),
  ann('Student', LABEL, { '@value': 'Étudiant', '@language': 'fr' }),
  ann('GraduateStudent', LABEL, { '@value': 'Graduate student', '@language': 'en-GB' }, [
    { property: COMMENT, value: { '@value': 'source' } },
  ]),
  ann('Student', COMMENT, { '@value': 'A student studies.', '@language': 'en' }),
  ann('Student', ALT, { '@value': 'pupil' }),
  ann('Student', 'http://example.org/count', { '@value': '3', '@type': XSD + 'integer' }),
  ann('Student', COMMENT, EX + 'Note'),
];

describe('find and replace', () => {
  it('replaces literal text case-insensitively in the labels and keeps language and annotations', () => {
    const r = run(AXIOMS, { find: 'student', replace: 'learner' });
    expect(r.changes.map((c) => (c.after!.value as { '@value': string })['@value'])).toEqual([
      'learner',
      'Graduate learner',
    ]);
    const g = r.changes[1];
    expect(g.before).toBe(AXIOMS[2]);
    expect(g.after).toEqual({
      ...AXIOMS[2],
      value: { '@value': 'Graduate learner', '@language': 'en-GB' },
    });
    // the comment is outside the label scope
    expect(r.changes.some((c) => c.property === COMMENT)).toBe(false);
  });

  it('takes regular expressions with groups, and a literal find treats them as text', () => {
    const r = run(AXIOMS, { find: '^(\\w+) student$', replace: '$1 learner', regex: true });
    expect(r.changes).toHaveLength(1);
    expect((r.changes[0].after!.value as { '@value': string })['@value']).toBe('Graduate learner');
    const lit = run([ann('X', LABEL, { '@value': 'a.b $1' })], { find: '.b $1', replace: '$&!' });
    expect((lit.changes[0].after!.value as { '@value': string })['@value']).toBe('a$&!');
  });

  it('honours case, the property scope and the language filter', () => {
    expect(
      run(AXIOMS, { find: 'student', caseSensitive: true, replace: 'x' }).changes,
    ).toHaveLength(1);
    const all = run(AXIOMS, { find: 'stud', replace: 'x', properties: { kind: 'all' } });
    expect(all.changes.map((c) => c.property).sort()).toEqual([COMMENT, LABEL, LABEL].sort());
    const one = run(AXIOMS, { find: 'p', replace: 'q', properties: { kind: 'one', iri: ALT } });
    expect(one.changes).toHaveLength(1);
    // `en` matches `en-GB` by basic filtering; untagged values are their own choice
    const en = run(AXIOMS, { find: 'u', replace: 'U', lang: { kind: 'tag', tag: 'en' } });
    expect(en.changes.map((c) => c.subject)).toEqual([EX + 'Student', EX + 'GraduateStudent']);
    const none = run(AXIOMS, { find: 'u', replace: 'U', lang: { kind: 'none' } });
    expect(none.changes.map((c) => c.property)).toEqual([ALT]);
    expect(langMatches('EN-us', { kind: 'tag', tag: 'en' })).toBe(true);
    expect(langMatches('eng', { kind: 'tag', tag: 'en' })).toBe(false);
  });

  it('leaves IRIs and typed literals alone and keeps to the entity list', () => {
    const r = run(AXIOMS, {
      find: '3|Note',
      regex: true,
      replace: 'x',
      properties: { kind: 'all' },
    });
    expect(r.changes).toHaveLength(0);
    const only = run(AXIOMS, {
      find: 'student',
      replace: 'x',
      entities: new Set([EX + 'GraduateStudent']),
    });
    expect(only.changes.map((c) => c.subject)).toEqual([EX + 'GraduateStudent']);
  });

  it('counts values that would become empty and leaves them out', () => {
    const r = run(AXIOMS, { find: 'pupil', replace: '', properties: { kind: 'all' } });
    expect(r.changes).toHaveLength(0);
    expect(r.emptied).toBe(1);
  });

  it('refuses a pattern that matches empty text inside a value', () => {
    for (const find of ['(?=.)', '\\b', 'x*']) {
      const s = spec({ find, regex: true, replace: 'X' });
      const re = compileFind(s);
      if (typeof re === 'string') continue;
      const r = findMatches([ann('A', LABEL, { '@value': 'abc' })], s, re);
      expect(r.zeroWidth, find).toBe('abc');
      expect(r.changes, find).toHaveLength(0);
    }
  });

  it('refuses an empty find, a bad pattern and a pattern that matches nothing at all', () => {
    expect(compileFind({ find: '', regex: false, caseSensitive: false })).toMatch(/Type/);
    expect(compileFind({ find: '(', regex: true, caseSensitive: false })).toMatch(/not valid/);
    expect(compileFind({ find: 'a*', regex: true, caseSensitive: false })).toMatch(/empty text/);
  });

  it('builds one axioms operation that removes each old assertion and adds its replacement', () => {
    const r = run(AXIOMS, { find: 'student', replace: 'learner' });
    const op = changesOp(r.changes);
    expect(op).toEqual({
      kind: 'axioms',
      remove: [AXIOMS[0], AXIOMS[2]],
      add: r.changes.map((c) => c.after),
    });
  });
});

describe('set and remove an annotation', () => {
  const v = { '@value': 'checked', '@language': 'en' };
  it('adds the value to each entity once and skips those that hold it', () => {
    const held = [ann('A', COMMENT, v, [{ property: LABEL, value: { '@value': 'x' } }])];
    const r = setAnnotation([EX + 'A', EX + 'B', EX + 'B'], COMMENT, v, 'add', held);
    expect(r.unchanged).toBe(1);
    expect(r.changes).toEqual([
      { subject: EX + 'B', property: COMMENT, before: null, after: ann('B', COMMENT, v) },
    ]);
    expect(changesOp(r.changes)).toEqual({
      kind: 'axioms',
      add: [ann('B', COMMENT, v)],
      remove: [],
    });
  });

  it("replaces the property's other values in the same language only", () => {
    const held = [
      ann('A', COMMENT, { '@value': 'old', '@language': 'en' }),
      ann('A', COMMENT, { '@value': 'vieux', '@language': 'fr' }),
      ann('B', COMMENT, v),
      ann('B', COMMENT, { '@value': 'stale', '@language': 'EN' }),
    ];
    const r = setAnnotation([EX + 'A', EX + 'B'], COMMENT, v, 'replace', held);
    const op = changesOp(r.changes);
    expect(op.kind === 'axioms' && op.remove).toEqual([held[0], held[3]]);
    expect(op.kind === 'axioms' && op.add).toEqual([ann('A', COMMENT, v)]);
    // an IRI value replaces every value of the property
    const iri = setAnnotation([EX + 'A'], COMMENT, EX + 'Doc', 'replace', held);
    expect(iri.changes.filter((c) => c.before).length).toBe(2);
  });

  it('keeps an annotated value whose language tag differs only in case', () => {
    const held = [
      ann('A', COMMENT, { '@value': 'checked', '@language': 'en' }, [
        { property: LABEL, value: { '@value': 'source' } },
      ]),
    ];
    const up = { '@value': 'checked', '@language': 'EN' };
    for (const mode of ['add', 'replace'] as const) {
      const r = setAnnotation([EX + 'A'], COMMENT, up, mode, held);
      expect(r.changes, mode).toEqual([]);
      expect(r.unchanged, mode).toBe(1);
    }
    // a plain literal and an xsd:string are one value
    const plain = [ann('A', COMMENT, { '@value': 'x' })];
    const typed = { '@value': 'x', '@type': XSD + 'string' };
    expect(setAnnotation([EX + 'A'], COMMENT, typed, 'replace', plain).changes).toEqual([]);
  });

  it('removes an IRI value without the literal of the same text, and the other way round', () => {
    const held = [
      ann('A', COMMENT, EX + 'Doc'),
      ann('A', COMMENT, { '@value': EX + 'Doc' }),
      ann('A', COMMENT, { '@value': EX + 'Doc', '@type': XSD + 'anyURI' }),
    ];
    const iri = removeAnnotation(
      [EX + 'A'],
      COMMENT,
      { kind: 'value', text: EX + 'Doc', iri: true },
      held,
    );
    expect(iri.map((c) => c.before)).toEqual([held[0]]);
    const lit = removeAnnotation(
      [EX + 'A'],
      COMMENT,
      { kind: 'value', text: EX + 'Doc', iri: false },
      held,
    );
    expect(lit.map((c) => c.before)).toEqual([held[1], held[2]]);
  });

  it('removes every value, those of a language or one exact text', () => {
    const held = [
      ann('A', COMMENT, { '@value': 'one', '@language': 'en' }),
      ann('A', COMMENT, { '@value': 'un', '@language': 'fr' }),
      ann('B', COMMENT, EX + 'Doc'),
      ann('C', COMMENT, { '@value': 'one', '@language': 'en' }),
      ann('A', LABEL, { '@value': 'one' }),
    ];
    const ents = [EX + 'A', EX + 'B'];
    expect(removeAnnotation(ents, COMMENT, { kind: 'any' }, held)).toHaveLength(3);
    const fr = removeAnnotation(
      ents,
      COMMENT,
      { kind: 'lang', lang: { kind: 'tag', tag: 'fr' } },
      held,
    );
    expect(fr.map((c) => c.before)).toEqual([held[1]]);
    const one = removeAnnotation(ents, COMMENT, { kind: 'value', text: 'one', iri: false }, held);
    expect(one.map((c) => c.before)).toEqual([held[0]]);
    const doc = removeAnnotation(
      ents,
      COMMENT,
      { kind: 'value', text: EX + 'Doc', iri: true },
      held,
    );
    expect(doc.map((c) => c.before)).toEqual([held[2]]);
  });

  it('reads a value form as a literal with a language or datatype, or as an IRI', () => {
    const px = { ex: EX };
    expect(valueOf('x', 'en', '', false, px)).toEqual({ '@value': 'x', '@language': 'en' });
    expect(valueOf('1', '', XSD + 'integer', false, px)).toEqual({
      '@value': '1',
      '@type': XSD + 'integer',
    });
    expect(valueOf('x', '', XSD + 'string', false, px)).toEqual({ '@value': 'x' });
    expect(valueOf('ex:Doc', '', '', true, px)).toBe(EX + 'Doc');
    expect(valueOf('not an iri', '', '', true, px)).toBeNull();
    expect(valueOf('', '', '', false, px)).toBeNull();
  });
});

describe('moving classes', () => {
  const restriction = {
    type: 'ObjectSomeValuesFrom',
    property: EX + 'teaches',
    class: EX + 'Course',
  };
  const HIER = [
    sub('Person', 'Agent'),
    sub('Student', 'Person'),
    sub('GraduateStudent', 'Student', [{ property: COMMENT, value: { '@value': 'why' } }]),
    sub('Employee', 'Person'),
    sub('Professor', 'Employee'),
    sub('Professor', restriction),
  ];

  it('finds the named parents and the descendants', () => {
    const p = namedParents(HIER);
    expect(p.get(EX + 'Professor')).toEqual([HIER[4]]);
    expect([...descendants(p, [EX + 'Person'])].sort()).toEqual(
      ['Employee', 'GraduateStudent', 'Professor', 'Student'].map((c) => EX + c),
    );
  });

  it('moves classes under a new parent, dropping named parents and keeping restrictions', () => {
    const r = planMove([EX + 'Professor', EX + 'GraduateStudent'], EX + 'Agent', HIER, false);
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.op).toEqual({
      kind: 'axioms',
      remove: [HIER[4], HIER[2]],
      add: [sub('Professor', 'Agent'), sub('GraduateStudent', 'Agent')],
      declare: false,
    });
  });

  it('can keep the old parents, move to the top, and skips classes already in place', () => {
    const keep = planMove([EX + 'Professor'], EX + 'Agent', HIER, true);
    expect(keep.ok && keep.op).toEqual({
      kind: 'axioms',
      remove: [],
      add: [sub('Professor', 'Agent')],
      declare: false,
    });
    const top = planMove([EX + 'Professor', EX + 'Agent'], null, HIER, false);
    expect(top.ok && top.op).toEqual({
      kind: 'axioms',
      remove: [HIER[4]],
      add: [],
      declare: false,
    });
    expect(top.ok && top.unchanged).toEqual([EX + 'Agent']);
    const there = planMove([EX + 'Student'], EX + 'Person', HIER, false);
    expect(there).toEqual({
      ok: false,
      reason: 'Every class is already where the move would put it.',
    });
  });

  it('refuses a move that would make a cycle', () => {
    const self = planMove([EX + 'Person'], EX + 'Person', HIER, false);
    expect(self.ok).toBe(false);
    const below = planMove([EX + 'Person', EX + 'Course'], EX + 'Professor', HIER, false, (i) =>
      i.slice(EX.length),
    );
    expect(below).toEqual({
      ok: false,
      reason: 'Professor is below Person, so the move would make a cycle.',
    });
  });

  it('refuses a move that would change more axioms than one edit lists', () => {
    const parents = Array.from({ length: 10_001 }, (_, i) => sub('Many', `P${i}`));
    const r = planMove([EX + 'Many'], EX + 'Agent', parents, false);
    expect(r.ok).toBe(false);
    expect(!r.ok && r.reason).toMatch(/10,002 SubClassOf axioms, more than the 10,000/);
  });

  it('refuses a move of more classes than the bound', () => {
    const many = Array.from({ length: BULK_LIMIT + 1 }, (_, i) => `${EX}C${i}`);
    const r = planMove(many, EX + 'Agent', [], false);
    expect(r.ok).toBe(false);
    expect(!r.ok && r.reason).toMatch(/1,001 classes, more than the 1,000/);
  });
});

describe('the entity list', () => {
  it('reads IRIs and prefixed names separated by lines, spaces or commas', () => {
    const r = parseEntityList(`ex:A\n<${EX}B>, ${EX}C  nope ex:A`, { ex: EX });
    expect(r.iris).toEqual([EX + 'A', EX + 'B', EX + 'C']);
    expect(r.bad).toEqual(['nope']);
  });
});

describe('the kinds of listed entities', () => {
  it('reads types in the ontology graphs only and maps them to kinds', () => {
    const OWL = 'http://www.w3.org/2002/07/owl#';
    const store = new ox.Store();
    const G = 'http://example.org/onto';
    store.load(
      `<${G}> {
        <${EX}C> a <${OWL}Class> .
        <${EX}p> a <${OWL}ObjectProperty> , <${OWL}TransitiveProperty> .
        <${EX}ada> a <${OWL}NamedIndividual> , <${EX}C> .
        <${EX}bob> a <${EX}C> .
        <${EX}pun> a <${OWL}Class> , <${OWL}NamedIndividual> .
      }
      <http://example.org/other> { <${EX}q> a <${OWL}DatatypeProperty> . }`,
      { format: 'application/trig' },
    );
    const iris = ['C', 'p', 'ada', 'bob', 'pun', 'q', 'unknown'].map((x) => EX + x);
    const rows = (store.query(typesQuery(iris, [G])) as Map<string, ox.Term>[]).map((m) => ({
      s: m.get('s')!.value,
      t: m.get('t')!.value,
    }));
    const k = kindsOfTypes(rows);
    expect(k.get(EX + 'C')).toEqual(['Class']);
    expect(k.get(EX + 'p')).toEqual(['ObjectProperty']);
    expect(k.get(EX + 'ada')).toEqual(['NamedIndividual']);
    expect(k.get(EX + 'bob')).toEqual(['NamedIndividual']);
    expect(k.get(EX + 'pun')?.sort()).toEqual(['Class', 'NamedIndividual']);
    // another graph than those the ontology reads, and an unknown IRI, give nothing
    expect(k.has(EX + 'q')).toBe(false);
    expect(k.has(EX + 'unknown')).toBe(false);
    expect(typesQuery([EX + 'C'], ['default'])).toContain('urn:x-arq:DefaultGraph');
  });
});
