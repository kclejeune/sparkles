import ox from 'oxigraph';
import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import { sessionBranchName } from './ontology';
import {
  aboveQuery,
  addProblem,
  ancestorsQuery,
  childrenQuery,
  ConceptTree,
  createConceptEdit,
  deleteEdit,
  detailsQuery,
  displayLabel,
  fromXlEdit,
  ambiguousXl,
  headUrl,
  labelMap,
  labelProblems,
  sameEntry,
  labelsQuery,
  linkEdit,
  makePreferredEdit,
  moveEdit,
  otherGraphsQuery,
  parseDetails,
  preferredResult,
  rootsQuery,
  schemesQuery,
  searchHits,
  searchQuery,
  toXlEdit,
  xlFormEdit,
  xlLabelIri,
  type LabelEntry,
} from './skos';

const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const XL = 'http://www.w3.org/2008/05/skos-xl#';
const EX = 'http://ex/';
const PREFIXES = `@prefix skos: <${SKOS}> . @prefix skosxl: <${XL}> . @prefix ex: <${EX}> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n`;

const plain = (kind: LabelEntry['kind'], value: string, lang = 'en'): LabelEntry => ({
  kind,
  label: { value, lang },
  xl: null,
});
const xl = (kind: LabelEntry['kind'], value: string, node: string, lang = 'en'): LabelEntry => ({
  kind,
  label: { value, lang },
  xl: { type: 'uri', value: node },
});
const rules = (es: LabelEntry[]) => labelProblems(es).map((p) => p.rule);

describe('the label checks repeat S13 and S14 for one resource', () => {
  it('finds the clashes of the SKOS Reference examples', () => {
    // Example 12
    expect(rules([plain('pref', 'love'), plain('pref', 'adoration')])).toEqual(['S14']);
    // Examples 13 to 15
    expect(rules([plain('pref', 'love'), plain('alt', 'love')])).toEqual(['S13']);
    expect(rules([plain('alt', 'love'), plain('hidden', 'love')])).toEqual(['S13']);
    expect(rules([plain('pref', 'love'), plain('hidden', 'love')])).toEqual(['S13']);
    // two untagged preferred labels
    expect(rules([plain('pref', 'love', ''), plain('pref', 'adoration', '')])).toEqual(['S14']);
  });

  it('passes the consistent examples', () => {
    expect(rules([plain('pref', 'love'), plain('alt', 'adoration')])).toEqual([]);
    expect(rules([plain('pref', 'love'), plain('pref', 'amour', 'fr')])).toEqual([]);
    expect(rules([plain('pref', 'love'), plain('alt', 'love', 'fr')])).toEqual([]);
  });

  it('counts SKOS-XL literal forms, as S55 to S57 make them labels (Examples 84 to 87)', () => {
    expect(rules([xl('pref', 'love', 'A'), xl('pref', 'adoration', 'B')])).toEqual(['S14']);
    expect(rules([xl('pref', 'love', 'A'), xl('alt', 'love', 'B')])).toEqual(['S13']);
    expect(rules([xl('alt', 'love', 'A'), xl('hidden', 'love', 'B')])).toEqual(['S13']);
    expect(rules([xl('pref', 'love', 'A'), xl('hidden', 'love', 'B')])).toEqual(['S13']);
    // a plain and an XL label in one language clash, the same label in both forms does not
    expect(rules([plain('pref', 'love'), xl('pref', 'adoration', 'A')])).toEqual(['S14']);
    expect(rules([plain('pref', 'love'), xl('pref', 'love', 'A')])).toEqual([]);
  });

  it('says what an added label would break', () => {
    const es = [plain('pref', 'love'), plain('alt', 'adoration')];
    expect(addProblem(es, 'pref', { value: 'amour', lang: 'fr' })).toBeNull();
    expect(addProblem(es, 'pref', { value: 'affection', lang: 'en' })).toMatch(/^S14/);
    expect(addProblem(es, 'hidden', { value: 'adoration', lang: 'en' })).toMatch(/^S13/);
    expect(addProblem(es, 'alt', { value: 'adoration', lang: 'en' })).toMatch(/already/);
    // an existing clash is not blamed on an unrelated new label
    const bad = [plain('pref', 'love'), plain('alt', 'love')];
    expect(addProblem(bad, 'alt', { value: 'fondness', lang: 'en' })).toBeNull();
  });

  it('shows the label in the wanted language, then untagged, then English', () => {
    const ls = [
      { value: 'Animaux', lang: 'fr' },
      { value: 'Animals', lang: 'en' },
    ];
    expect(displayLabel(ls, ['fr-CA'])).toBe('Animaux');
    expect(displayLabel(ls, ['de'])).toBe('Animals');
    expect(displayLabel([{ value: 'x', lang: '' }, ...ls], ['de'])).toBe('x');
  });
});

// --- against a store ------------------------------------------------------------------

function store(turtle: string) {
  const s = new ox.Store();
  s.load(PREFIXES + turtle, { format: 'text/turtle' });
  return s;
}

const term = (t: ox.Term): Term =>
  t.termType === 'NamedNode'
    ? { type: 'uri', value: t.value }
    : t.termType === 'BlankNode'
      ? { type: 'bnode', value: t.value }
      : {
          type: 'literal',
          value: t.value,
          ...((t as ox.Literal).language ? { 'xml:lang': (t as ox.Literal).language } : {}),
        };

function select(s: ox.Store, q: string): Record<string, Term | undefined>[] {
  const out = s.query(q) as Map<string, ox.Term>[];
  return out.map((m) => Object.fromEntries([...m].map(([k, v]) => [k, term(v)])));
}
const ask = (s: ox.Store, q: string) => s.query(q) as boolean;
const quads = (s: ox.Store) =>
  s
    .match()
    .map((q) => q.toString())
    .sort();

const THES = `
ex:s a skos:ConceptScheme ; skos:prefLabel "Animals"@en ; skos:hasTopConcept ex:animal .
ex:animal a skos:Concept ; skos:topConceptOf ex:s ; skos:inScheme ex:s ;
  skos:prefLabel "Animal"@en , "Animal"@fr ; skos:narrower ex:mammal , ex:marine .
ex:mammal a skos:Concept ; skos:inScheme ex:s ; skos:broader ex:animal ; skos:prefLabel "Mammal"@en .
ex:marine a skos:Concept ; skos:inScheme ex:s ; skos:broader ex:animal ; skos:prefLabel "Marine"@en .
ex:whale a skos:Concept ; skos:inScheme ex:s ; skos:broader ex:mammal ; skos:prefLabel "Whale"@en ;
  skos:altLabel "Cetacean"@en ; skosxl:prefLabel ex:whale-fr ; skos:exactMatch <http://other/whale> .
ex:marine skos:narrower ex:whale .
ex:whale-fr a skosxl:Label ; skosxl:literalForm "Baleine"@fr .
ex:loose a skos:Concept ; skos:inScheme ex:s ; skos:prefLabel "Loose"@en .
ex:list a skos:OrderedCollection ; skos:memberList ( ex:whale ex:mammal ) .
<urn:u:s> <urn:u:p> "kept" .
`;

describe('the reads', () => {
  it('reads the schemes, the roots and one level of children with their counts', () => {
    const s = store(THES);
    expect(select(s, schemesQuery(null)).map((r) => r.s?.value)).toEqual([EX + 's']);
    const t = new ConceptTree();
    t.setRoots(select(s, rootsQuery(EX + 's', null)));
    expect(t.roots).toEqual([EX + 'animal', EX + 'loose']);
    expect(t.top.has(EX + 'animal')).toBe(true);
    expect(t.top.has(EX + 'loose')).toBe(false);
    expect(t.count(EX + 'animal')).toBe(2);
    t.addChildren([EX + 'animal'], select(s, childrenQuery([EX + 'animal'], null)));
    expect(t.children.get(EX + 'animal')).toEqual([EX + 'mammal', EX + 'marine']);
    t.addChildren(
      [EX + 'mammal', EX + 'marine'],
      select(s, childrenQuery([EX + 'mammal', EX + 'marine'], null)),
    );
    // the whale sits under both of its broader concepts
    expect(t.children.get(EX + 'mammal')).toEqual([EX + 'whale']);
    expect(t.children.get(EX + 'marine')).toEqual([EX + 'whale']);
    expect(t.parents.get(EX + 'whale')?.size).toBe(2);
  });

  it('reads the ancestors and refuses a move under a narrower concept', () => {
    const s = store(THES);
    const anc = select(s, ancestorsQuery(EX + 'whale', null)).map((r) => r.a?.value);
    expect(anc.sort()).toEqual([EX + 'animal', EX + 'mammal', EX + 'marine']);
    expect(ask(s, aboveQuery(EX + 'animal', EX + 'whale', null))).toBe(true);
    expect(ask(s, aboveQuery(EX + 'whale', EX + 'loose', null))).toBe(false);
  });

  it('reads labels plain and SKOS-XL, and finds them in any language', () => {
    const s = store(THES);
    const m = labelMap(select(s, labelsQuery([EX + 'whale'], null)));
    expect(
      m
        .get(EX + 'whale')
        ?.map((l) => l.value)
        .sort(),
    ).toEqual(['Baleine', 'Whale']);
    const fr = searchHits(select(s, searchQuery('bal', { graph: null })), 'bal');
    expect(fr.map((h) => [h.iri, h.kind, h.label.lang])).toEqual([[EX + 'whale', 'pref', 'fr']]);
    const alt = searchHits(select(s, searchQuery('ceta', { graph: null, lang: 'en' })), 'ceta');
    expect(alt.map((h) => h.kind)).toEqual(['alt']);
    expect(select(s, searchQuery('ceta', { graph: null, lang: 'fr' }))).toEqual([]);
  });

  it('parses the details of a concept with both directions of its links', () => {
    const s = store(THES);
    const d = parseDetails(EX + 'whale', select(s, detailsQuery(EX + 'whale', null)));
    expect(d.types).toEqual([SKOS + 'Concept']);
    expect(d.broader.map((l) => [l.iri, l.forward, l.inverse])).toEqual([
      [EX + 'mammal', true, false],
      // stated as marine's narrower only
      [EX + 'marine', false, true],
    ]);
    expect(d.labels.map((e) => [e.kind, e.label.value, !!e.xl]).sort()).toEqual([
      ['alt', 'Cetacean', false],
      ['pref', 'Baleine', true],
      ['pref', 'Whale', false],
    ]);
    expect(d.mappings).toEqual([{ property: 'exactMatch', target: 'http://other/whale' }]);
    const a = parseDetails(EX + 'animal', select(s, detailsQuery(EX + 'animal', null)));
    expect(a.topConceptOf.map((l) => [l.iri, l.forward, l.inverse])).toEqual([
      [EX + 's', true, true],
    ]);
  });
});

describe('the updates', () => {
  it('moves a concept with both directions of the hierarchy', () => {
    const s = store(THES);
    s.update(
      moveEdit(EX + 'mammal', { parent: EX + 'animal' }, { parent: EX + 'loose' }, false, null),
    );
    expect(ask(s, `ASK { <${EX}mammal> <${SKOS}broader> <${EX}animal> }`)).toBe(false);
    expect(ask(s, `ASK { <${EX}animal> <${SKOS}narrower> <${EX}mammal> }`)).toBe(false);
    expect(ask(s, `ASK { <${EX}mammal> <${SKOS}broader> <${EX}loose> }`)).toBe(true);
    expect(ask(s, `ASK { <${EX}loose> <${SKOS}narrower> <${EX}mammal> }`)).toBe(true);
    // copy keeps the old place: a second broader concept
    s.update(moveEdit(EX + 'loose', null, { parent: EX + 'animal' }, true, null));
    s.update(moveEdit(EX + 'loose', { parent: EX + 'animal' }, { top: EX + 's' }, true, null));
    const d = parseDetails(EX + 'loose', select(s, detailsQuery(EX + 'loose', null)));
    expect(d.broader.map((l) => l.iri)).toEqual([EX + 'animal']);
    expect(d.topConceptOf.map((l) => [l.iri, l.forward, l.inverse])).toEqual([
      [EX + 's', true, true],
    ]);
  });

  it('creates a concept under a parent and deletes it with its XL labels, keeping lists', () => {
    const s = store(THES);
    const before = quads(s);
    s.update(
      createConceptEdit(
        EX + 'orca',
        { value: 'Orca', lang: 'en' },
        { scheme: EX + 's', at: { parent: EX + 'marine' } },
        null,
      ),
    );
    expect(ask(s, `ASK { <${EX}marine> <${SKOS}narrower> <${EX}orca> }`)).toBe(true);
    s.update(deleteEdit(EX + 'orca', null));
    expect(quads(s)).toEqual(before);
    // the whale goes with its XL label, and the ordered list keeps its cells
    s.update(deleteEdit(EX + 'whale', null));
    expect(ask(s, `ASK { <${EX}whale-fr> ?p ?o }`)).toBe(false);
    expect(
      ask(s, `ASK { ?l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <${EX}whale> }`),
    ).toBe(true);
    expect(ask(s, `ASK { <urn:u:s> <urn:u:p> "kept" }`)).toBe(true);
  });

  it('makes a label preferred and the old one alternative', () => {
    const s = store(THES);
    const d = parseDetails(EX + 'whale', select(s, detailsQuery(EX + 'whale', null)));
    const cetacean = d.labels.find((l) => l.label.value === 'Cetacean' && !l.xl)!;
    s.update(makePreferredEdit(EX + 'whale', cetacean, d.labels, null));
    const e = parseDetails(EX + 'whale', select(s, detailsQuery(EX + 'whale', null)));
    expect(
      e.labels
        .filter((l) => !l.xl)
        .map((l) => [l.kind, l.label.value])
        .sort(),
    ).toEqual([
      ['alt', 'Whale'],
      ['pref', 'Cetacean'],
    ]);
    expect(labelProblems(e.labels)).toEqual([]);
  });

  it('converts plain labels to SKOS-XL and back, leaving the graph as it was', () => {
    const s = store(THES);
    const before = quads(s);
    const node = xlLabelIri(EX + 'whale', 'alt', { value: 'Cetacean', lang: 'en' }, new Set());
    expect(node).toBe(`${EX}whale-xl-alt-en`);
    s.update(toXlEdit(EX + 'whale', 'alt', { value: 'Cetacean', lang: 'en' }, node, null));
    const d = parseDetails(EX + 'whale', select(s, detailsQuery(EX + 'whale', null)));
    expect(d.labels.find((l) => l.label.value === 'Cetacean')?.xl).toEqual({
      type: 'uri',
      value: node,
    });
    s.update(
      fromXlEdit(
        EX + 'whale',
        'alt',
        { type: 'uri', value: node },
        { value: 'Cetacean', lang: 'en' },
        { value: 'Cetacean', lang: 'en' },
        null,
      ),
    );
    expect(quads(s)).toEqual(before);
  });

  it('edits and removes a blank-node XL label by its literal form', () => {
    const s =
      store(`ex:c a skos:Concept ; skosxl:prefLabel [ a skosxl:Label ; skosxl:literalForm "Chat"@fr ] ,
      [ a skosxl:Label ; skosxl:literalForm "Cat"@en ] .`);
    const d = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    const chat = d.labels.find((l) => l.label.value === 'Chat')!;
    expect(chat.xl?.type).toBe('bnode');
    s.update(
      xlFormEdit(EX + 'c', 'pref', chat.xl!, chat.label, { value: 'Chatte', lang: 'fr' }, null),
    );
    let e = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    expect(e.labels.map((l) => l.label.value).sort()).toEqual(['Cat', 'Chatte']);
    const chatte = e.labels.find((l) => l.label.value === 'Chatte')!;
    s.update(fromXlEdit(EX + 'c', 'pref', chatte.xl!, chatte.label, null, null));
    e = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    expect(e.labels.map((l) => l.label.value)).toEqual(['Cat']);
    // the other label's resource is untouched
    expect(s.size).toBe(4);
  });

  it('writes related links in both directions and in a named graph', () => {
    const s = new ox.Store();
    s.load(PREFIXES + 'ex:a a skos:Concept . ex:b a skos:Concept .', {
      format: 'text/turtle',
      to_graph_name: ox.namedNode('urn:g'),
    });
    s.update(linkEdit(EX + 'a', 'related', EX + 'b', false, 'urn:g'));
    expect(s.match(null, ox.namedNode(SKOS + 'related'), null, ox.namedNode('urn:g')).length).toBe(
      2,
    );
    expect(s.match(null, null, null, ox.defaultGraph()).length).toBe(0);
    s.update(linkEdit(EX + 'a', 'related', EX + 'b', true, 'urn:g'));
    expect(s.match(null, ox.namedNode(SKOS + 'related'), null, null).length).toBe(0);
  });
});

describe('label edits keep what other resources and labels need', () => {
  it('keeps a SKOS-XL label resource that another concept still uses', () => {
    const s = store(`ex:a skosxl:altLabel ex:shared . ex:b skosxl:altLabel ex:shared .
      ex:shared a skosxl:Label ; skosxl:literalForm "Feline"@en .`);
    const node = { type: 'uri', value: EX + 'shared' } as const;
    s.update(fromXlEdit(EX + 'a', 'alt', node, { value: 'Feline', lang: 'en' }, null, null));
    expect(ask(s, `ASK { <${EX}a> ?p ?o }`)).toBe(false);
    expect(ask(s, `ASK { <${EX}shared> <${XL}literalForm> "Feline"@en }`)).toBe(true);
    s.update(deleteEdit(EX + 'b', null));
    // the last user removed it: the label goes too
    expect(s.size).toBe(0);
  });

  it('keeps a shared label when its concept is deleted', () => {
    const s = store(`ex:a skosxl:altLabel ex:shared . ex:b skosxl:altLabel ex:shared .
      ex:shared a skosxl:Label ; skosxl:literalForm "Feline"@en .`);
    s.update(deleteEdit(EX + 'a', null));
    expect(ask(s, `ASK { <${EX}shared> <${XL}literalForm> "Feline"@en }`)).toBe(true);
  });

  it('tells blank-node twins apart only when their forms differ', () => {
    const s = store(`ex:c skosxl:altLabel [ skosxl:literalForm "Cat"@en ; ex:note "one" ] ,
      [ skosxl:literalForm "Cat"@en ; ex:note "two" ] , [ skosxl:literalForm "Kitty"@en ] .`);
    const d = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    const cats = d.labels.filter((l) => l.label.value === 'Cat');
    expect(cats.length).toBe(2);
    expect(cats.every((c) => ambiguousXl(d.labels, c))).toBe(true);
    expect(
      ambiguousXl(
        d.labels,
        d.labels.find((l) => l.label.value === 'Kitty')!,
      ),
    ).toBe(false);
  });

  it('makes a label preferred over a SKOS-XL preferred label, which becomes alternative', () => {
    const s = store(`ex:c skosxl:prefLabel ex:cat ; skos:altLabel "feline"@en .
      ex:cat a skosxl:Label ; skosxl:literalForm "cat"@en .`);
    const d = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    const feline = d.labels.find((l) => l.label.value === 'feline')!;
    s.update(makePreferredEdit(EX + 'c', feline, d.labels, null));
    const e = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    expect(e.labels.map((l) => [l.kind, l.label.value, !!l.xl]).sort()).toEqual([
      ['alt', 'cat', true],
      ['pref', 'feline', false],
    ]);
    expect(labelProblems(e.labels)).toEqual([]);
    // and back: the XL label is promoted by its link, the plain one demoted
    const cat = e.labels.find((l) => l.label.value === 'cat')!;
    s.update(makePreferredEdit(EX + 'c', cat, e.labels, null));
    const f = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    expect(f.labels.map((l) => [l.kind, l.label.value, !!l.xl]).sort()).toEqual([
      ['alt', 'feline', false],
      ['pref', 'cat', true],
    ]);
  });

  it('promotes every representation of the chosen literal', () => {
    const s = store(`ex:c skos:prefLabel "cat"@en ; skos:altLabel "feline"@en ;
      skosxl:altLabel ex:feline . ex:feline a skosxl:Label ; skosxl:literalForm "feline"@en .`);
    const d = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    const xlFeline = d.labels.find((l) => l.label.value === 'feline' && l.xl)!;
    const after = preferredResult(xlFeline, d.labels);
    expect(labelProblems(after)).toEqual([]);
    s.update(makePreferredEdit(EX + 'c', xlFeline, d.labels, null));
    const e = parseDetails(EX + 'c', select(s, detailsQuery(EX + 'c', null)));
    expect(e.labels.map((l) => [l.kind, l.label.value, !!l.xl]).sort()).toEqual([
      ['alt', 'cat', false],
      ['pref', 'feline', false],
      ['pref', 'feline', true],
    ]);
    expect(labelProblems(e.labels)).toEqual([]);
  });

  it('checks an edit without the unedited twin of the label', () => {
    const es = [plain('pref', 'cat'), xl('pref', 'cat', EX + 'cat')];
    const rest = es.filter((e) => !sameEntry(e, es[0]));
    expect(addProblem(rest, 'pref', { value: 'feline', lang: 'en' })).toMatch(/S14/);
  });
});

describe('the graphs outside the editor', () => {
  it('are found in named graphs and, for a named graph, in the default graph', () => {
    const s = new ox.Store();
    s.load(PREFIXES + 'ex:a a skos:Concept .', {
      format: 'text/turtle',
      to_graph_name: ox.namedNode('urn:g'),
    });
    expect(ask(s, otherGraphsQuery('urn:g'))).toBe(false);
    expect(ask(s, otherGraphsQuery(null))).toBe(true);
    // a default graph that is the union of the named graphs holds nothing of its own
    expect(s.query(otherGraphsQuery('urn:g'), { use_default_graph_as_union: true })).toBe(false);
    s.load(PREFIXES + 'ex:b a skos:Concept .', { format: 'text/turtle' });
    expect(ask(s, otherGraphsQuery('urn:g'))).toBe(true);
    expect(s.query(otherGraphsQuery('urn:g'), { use_default_graph_as_union: true })).toBe(true);
  });
});

describe('the head', () => {
  it('is read on the graph the editor shows', () => {
    expect(headUrl('thes@dev', null)).toBe('/thes%40dev/data?default');
    expect(headUrl('thes', 'urn:g')).toBe('/thes/data?graph=urn%3Ag');
  });
});

describe('the session branch', () => {
  it('takes the thesaurus prefix', () => {
    expect(sessionBranchName('ada', new Date('2026-11-02T10:00:00Z'), 'skos')).toBe(
      'skos.ada.20261102',
    );
    expect(sessionBranchName('ada', new Date('2026-11-02T10:00:00Z'))).toBe('onto.ada.20261102');
  });
});
