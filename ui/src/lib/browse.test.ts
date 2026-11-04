import { describe, expect, it } from 'vitest';
import {
  axiomGroups,
  browseHref,
  builtIn,
  countsQuery,
  definitionsQuery,
  displayName,
  downloadHref,
  editHref,
  frameText,
  inferredOf,
  manchesterFrame,
  incomingQuery,
  kindWord,
  langRank,
  lineage,
  linkCuries,
  logicalDefinitionOf,
  mentionGroups,
  mergePieces,
  namesQuery,
  neighbourhood,
  hierarchyOf,
  hiddenRows,
  xrefsByDatabase,
  oboIri,
  parseCounts,
  parseDefinitions,
  parseHead,
  parseReuse,
  synonymTypeName,
  languageCount,
  termSections,
  textBlocks,
  treeKindOf,
  xrefHref,
  usageQuery,
  valueText,
  type Statement,
  type TermPage,
  type TreeNode,
} from './browse';
import type { AxiomItem } from './ontology';

const EX = 'http://example.org/';
const RDF = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const OWL = 'http://www.w3.org/2002/07/owl#';
const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const OIO = 'http://www.geneontology.org/formats/oboInOwl#';
const OBO = 'http://purl.obolibrary.org/obo/';

const lit = (v: string, lang?: string) => ({ '@value': v, ...(lang ? { '@language': lang } : {}) });
const st = (
  property: string,
  value: Statement['value'],
  annotations: Statement['annotations'] = [],
): Statement => ({
  property,
  value,
  graph: 'default',
  annotations,
});
const item = (axiom: Record<string, unknown>): AxiomItem =>
  ({ type: String(axiom.type), ofn: '', axiom, graph: 'default' }) as unknown as AxiomItem;
const node = (iri: string, label: string, children = 0, deprecated = false): TreeNode => ({
  iri,
  label,
  labelLang: null,
  shortForm: null,
  kinds: ['class'],
  deprecated,
  children,
  parents: 1,
});

describe('the sections of a page', () => {
  it('lists every definition, description, comment and scope note under its predicate', () => {
    const DCT = 'http://purl.org/dc/terms/';
    const IAO = OBO + 'IAO_0000115';
    const of = (stmts: Statement[], langs = ['en']) =>
      termSections({ statements: stmts }, langs).lead.groups.map((g) => [
        g.kind,
        g.predicates,
        g.values.map((v) => [valueText(v.value), v.lang]),
        g.translations.map((v) => v.lang),
      ]);
    // two English definitions from different predicates: a group for each
    expect(
      of([st(SKOS + 'definition', lit('skos text', 'en')), st(IAO, lit('obo text', 'en'))]),
    ).toEqual([
      ['Definition', [IAO], [['obo text', 'en']], []],
      ['Definition', [SKOS + 'definition'], [['skos text', 'en']], []],
    ]);
    // English and French of one predicate: the French value folds
    expect(of([st(IAO, lit('texte', 'fr')), st(IAO, lit('text', 'en'))])).toEqual([
      ['Definition', [IAO], [['text', 'en']], ['fr']],
    ]);
    // the same text from both predicates is one entry under a heading that names both
    expect(of([st(SKOS + 'definition', lit('same', 'en')), st(IAO, lit('same', 'en'))])).toEqual([
      ['Definition', [IAO, SKOS + 'definition'], [['same', 'en']], []],
    ]);
    // a definition, a description, a comment and a scope note, in priority order
    expect(
      of([
        st(SKOS + 'scopeNote', lit('scope')),
        st(RDFS + 'comment', lit('comment')),
        st(DCT + 'description', lit('description')),
        st(IAO, lit('definition')),
      ]),
    ).toEqual([
      ['Definition', [IAO], [['definition', null]], []],
      ['Description', [DCT + 'description'], [['description', null]], []],
      ['Comment', [RDFS + 'comment'], [['comment', null]], []],
      ['Scope note', [SKOS + 'scopeNote'], [['scope', null]], []],
    ]);
    // nothing in the block: a note leads it and leaves the notes, which name their predicate
    const n = termSections(
      { statements: [st(SKOS + 'note', lit('first')), st(SKOS + 'example', lit('e.g.'))] },
      ['en'],
    );
    expect(n.lead.groups.map((g) => [g.kind, g.predicates])).toEqual([['Note', [SKOS + 'note']]]);
    expect(n.notes.map((g) => [g.kind, g.property])).toEqual([['Example', SKOS + 'example']]);
    expect(
      termSections({ statements: [st(RDFS + 'label', lit('x'))] }, ['en']).definition,
    ).toBeNull();
  });

  it('folds the translations of a UNESCO scope note under its heading', () => {
    const notes = ['en', 'fr', 'es', 'ru', 'ar'].map((l) =>
      st(SKOS + 'scopeNote', lit(`note ${l}`, l)),
    );
    const [g] = termSections({ statements: notes }, ['en']).lead.groups;
    expect([g.kind, g.predicates]).toEqual(['Scope note', [SKOS + 'scopeNote']]);
    expect(g.values.map((v) => valueText(v.value))).toEqual(['note en']);
    expect(g.translations.map((v) => v.lang).sort()).toEqual(['ar', 'es', 'fr', 'ru']);
    // a reader of French gets the French note first
    const [fr] = termSections({ statements: notes }, ['fr']).lead.groups;
    expect(valueText(fr.values[0].value)).toBe('note fr');
    // a kind with only translations shows its best one
    const [only] = termSections({ statements: notes.slice(1) }, ['en']).lead.groups;
    expect(only.values).toHaveLength(1);
    expect(only.translations).toHaveLength(3);
  });

  it('takes only class expressions as a logical definition', () => {
    const iri = EX + 'C';
    const expr = { type: 'ObjectIntersectionOf', classes: [EX + 'A', EX + 'B'] };
    const both = axiomGroups(
      iri,
      ['class'],
      [
        item({ type: 'EquivalentClasses', classes: [iri, EX + 'Alias'] }),
        item({ type: 'EquivalentClasses', classes: [iri, expr] }),
      ],
    );
    expect(logicalDefinitionOf(both)).toEqual([expr]);
    const alias = axiomGroups(
      iri,
      ['class'],
      [item({ type: 'EquivalentClasses', classes: [iri, EX + 'Alias'] })],
    );
    expect(logicalDefinitionOf(alias)).toEqual([]);
  });

  it('keeps a description’s paragraphs and line breaks', () => {
    expect(textBlocks('One line.')).toEqual([['One line.']]);
    expect(
      textBlocks('\n  First paragraph,\n  its second line.\n\n\nSecond.  \r\n\r\nThird.\n'),
    ).toEqual([['First paragraph,', 'its second line.'], ['Second.'], ['Third.']]);
    expect(textBlocks('   ')).toEqual([]);
  });

  it('names a synonym’s type by its label, else its local name', () => {
    const CL = 'http://purl.obolibrary.org/obo/cl#';
    const s = termSections(
      {
        statements: [
          st(OIO + 'hasRelatedSynonym', lit('microglia'), [
            { property: OIO + 'hasSynonymType', value: CL + 'PLURAL' },
            { property: OIO + 'hasDbXref', value: lit('X:1') },
          ]),
          st(OIO + 'hasRelatedSynonym', lit('MG'), [
            { property: OIO + 'hasSynonymType', value: CL + 'ABBREVIATION' },
          ]),
        ],
      },
      ['en'],
    );
    const vs = s.synonyms[0].values;
    expect(vs.map((v) => [valueText(v.value), v.types, v.sources])).toEqual([
      ['MG', [CL + 'ABBREVIATION'], []],
      ['microglia', [CL + 'PLURAL'], ['X:1']],
    ]);
    const names = {
      [CL + 'PLURAL']: {
        label: 'plural',
        labelLang: 'en',
        shortForm: null,
        kinds: [],
        deprecated: false,
      },
    };
    expect(synonymTypeName(CL + 'PLURAL', names)).toBe('plural');
    expect(synonymTypeName(CL + 'ABBREVIATION', names)).toBe('ABBREVIATION');
  });

  it('keeps a SKOS concept’s facts with the other statements', () => {
    const p = EX + 'custom';
    const names = {
      [p]: {
        kinds: ['dataProperty'],
        label: null,
        labelLang: null,
        shortForm: null,
        deprecated: false,
      },
    };
    const stmts = [st(p, lit('v'))];
    expect(termSections({ statements: stmts, names }, ['en']).other).toEqual([]);
    expect(
      termSections({ statements: stmts, names, kinds: ['concept'] }, ['en']).other.map(
        (o) => o.property,
      ),
    ).toEqual([p]);
  });

  it('sorts statements into definition, synonyms, notes, xrefs and the rest', () => {
    const s = termSections(
      {
        statements: [
          st(OBO + 'IAO_0000115', lit('Une définition', 'fr')),
          st(OBO + 'IAO_0000115', lit('A definition'), [
            { property: OIO + 'hasDbXref', value: lit('GOC:al') },
          ]),
          st(OIO + 'hasExactSynonym', lit('mitochondria')),
          st(OIO + 'hasRelatedSynonym', lit('mito')),
          st(RDFS + 'comment', lit('See GO:0032047.')),
          st(OIO + 'hasDbXref', lit('Wikipedia:Mitochondrion')),
          st(OIO + 'hasDbXref', lit('NIF:sao1')),
          st(OIO + 'id', lit('GO:0005739')),
          st(RDFS + 'subClassOf', EX + 'Parent'),
          st(EX + 'custom', lit('kept')),
          st(EX + 'fact', EX + 'Other'),
        ],
        names: {
          [EX + 'fact']: {
            label: null,
            labelLang: null,
            shortForm: null,
            kinds: ['objectProperty'],
            deprecated: false,
          },
        },
      },
      ['en'],
    );
    expect(s.definition?.value).toEqual(lit('A definition'));
    expect(s.definition?.sources).toEqual(['GOC:al']);
    // the comment joins the lead block and the French definition folds as a translation
    expect(s.lead.groups.map((g) => [g.kind, g.translations.map((v) => v.lang)])).toEqual([
      ['Definition', ['fr']],
      ['Comment', []],
    ]);
    expect(s.synonyms.map((g) => g.kind)).toEqual(['Exact', 'Related']);
    expect(s.notes).toEqual([]);
    expect(s.xrefs.map((x) => [x.database, x.id, x.href])).toEqual([
      ['NIF', 'sao1', null],
      ['Wikipedia', 'Mitochondrion', 'https://en.wikipedia.org/wiki/Mitochondrion'],
    ]);
    expect(s.info).toEqual([{ kind: 'ID', values: [lit('GO:0005739')] }]);
    // axioms in the RDF vocabularies and facts of object properties are shown elsewhere
    expect(s.other).toEqual([{ property: EX + 'custom', values: [lit('kept')] }]);
  });

  it('builds a table of SKOS labels by language, in the caller’s order', () => {
    const s = termSections(
      {
        statements: [
          st(SKOS + 'prefLabel', lit('Biodiversité', 'fr')),
          st(SKOS + 'prefLabel', lit('Biodiversity', 'en')),
          st(SKOS + 'altLabel', lit('Biological diversity', 'en')),
          st(SKOS + 'hiddenLabel', lit('biodiv', 'en')),
          st(SKOS + 'related', EX + 'Genetics'),
          st(SKOS + 'exactMatch', EX + 'other'),
        ],
      },
      ['fr'],
    );
    expect(s.labels).toEqual([
      { lang: 'fr', preferred: ['Biodiversité'], alternative: [], hidden: [] },
      {
        lang: 'en',
        preferred: ['Biodiversity'],
        alternative: ['Biological diversity'],
        hidden: ['biodiv'],
      },
    ]);
    expect(s.related).toEqual([EX + 'Genetics']);
    expect(s.mappings).toEqual([{ kind: 'Exact match', values: [EX + 'other'] }]);
  });

  it('lists a value once when several graphs state it', () => {
    const twice = (x: Statement): Statement[] => [x, { ...x, graph: 'urn:other' }];
    const s = termSections(
      {
        statements: [
          st(OBO + 'IAO_0000115', lit('A definition')),
          {
            ...st(OBO + 'IAO_0000115', lit('A definition'), [
              { property: OIO + 'hasDbXref', value: lit('GOC:al') },
            ]),
            graph: 'urn:other',
          },
          ...twice(st(OIO + 'hasExactSynonym', lit('mitochondria'))),
          ...twice(st(OIO + 'hasDbXref', lit('Wikipedia:Mitochondrion'))),
          ...twice(st(OIO + 'id', lit('GO:0005739'))),
          ...twice(st(SKOS + 'related', EX + 'Genetics')),
          ...twice(st(RDFS + 'seeAlso', EX + 'page')),
          ...twice(st(EX + 'custom', lit('kept'))),
        ],
      },
      ['en'],
    );
    // the sources of every copy stay with the one value
    expect(s.definition?.sources).toEqual(['GOC:al']);
    expect(s.lead.groups.map((g) => g.values.length)).toEqual([1]);
    expect(s.synonyms[0].values).toHaveLength(1);
    expect(s.xrefs).toHaveLength(1);
    expect(s.info[0].values).toHaveLength(1);
    expect(s.related).toEqual([EX + 'Genetics']);
    expect(s.seeAlso).toHaveLength(1);
    expect(s.other).toEqual([{ property: EX + 'custom', values: [lit('kept')] }]);
  });

  it('reads the literal forms of SKOS-XL labels as SKOS labels', () => {
    const XL = 'http://www.w3.org/2008/05/skos-xl#';
    const s = termSections(
      {
        statements: [st(SKOS + 'prefLabel', lit('Biodiversity', 'en'))],
        xlLabels: [
          { property: XL + 'prefLabel', value: lit('Biodiversity', 'en'), graph: 'default' },
          { property: XL + 'prefLabel', value: lit('Biodiversité', 'fr'), graph: 'default' },
          { property: XL + 'altLabel', value: lit('Biological diversity', 'en'), graph: 'default' },
        ],
      },
      ['en'],
    );
    expect(s.labels).toEqual([
      {
        lang: 'en',
        preferred: ['Biodiversity'],
        alternative: ['Biological diversity'],
        hidden: [],
      },
      { lang: 'fr', preferred: ['Biodiversité'], alternative: [], hidden: [] },
    ]);
    expect(s.synonyms).toEqual([
      expect.objectContaining({
        kind: 'Alternative',
        values: [expect.objectContaining({ lang: 'en' })],
      }),
    ]);
  });

  it('ranks languages: the caller’s, then none, then English, then the rest', () => {
    const l = ['de'];
    expect(langRank('de-AT', l)).toBe(0);
    expect(langRank(null, l)).toBe(1);
    expect(langRank('en', l)).toBe(2);
    expect(langRank('fr', l)).toBe(3);
  });

  it('resolves well-known cross-references and leaves others as text', () => {
    expect(xrefHref('PMID:123')).toBe('https://pubmed.ncbi.nlm.nih.gov/123/');
    expect(xrefHref('DOI:10.1/x')).toBe('https://doi.org/10.1/x');
    expect(xrefHref('https://example.org/a')).toBe('https://example.org/a');
    expect(xrefHref('ISBN:0198506732')).toBeNull();
    expect(xrefHref('nocolon')).toBeNull();
  });

  it('links CURIEs in text that a resolver knows', () => {
    const parts = linkCuries('use GO:0032047 or CHEBI:15377 here', (c) =>
      c.startsWith('GO:') ? oboIri(c) : null,
    );
    expect(parts).toEqual([
      { text: 'use ' },
      { text: 'GO:0032047', iri: OBO + 'GO_0032047' },
      { text: ' or CHEBI:15377 here' },
    ]);
    expect(oboIri('not a curie')).toBeNull();
  });
});

const OWL_THING = 'http://www.w3.org/2002/07/owl#Thing';

describe('axioms', () => {
  it('groups the axioms about a class by the frame’s sections and leaves the rest last', () => {
    const iri = EX + 'C';
    const g = axiomGroups(
      iri,
      ['class'],
      [
        item({ type: 'Declaration', kind: 'Class', iri }),
        item({ type: 'DisjointClasses', classes: [iri, EX + 'D'] }),
        item({ type: 'SubClassOf', subClass: iri, superClass: EX + 'P' }),
        item({ type: 'SubClassOf', subClass: EX + 'Child', superClass: iri }),
        item({ type: 'HasKey', class: EX + 'Other', objectProperties: [], dataProperties: [] }),
      ],
    );
    expect(g.map((x) => [x.id, x.title, x.items.length])).toEqual([
      ['sub', 'Subclass of', 1],
      ['disjoint', 'Disjoint with', 1],
      ['other', 'Other axioms', 1],
    ]);
    expect(g[1].items[0].values).toEqual([EX + 'D']);
  });

  it('collects the characteristics of a property', () => {
    const p = EX + 'p';
    const g = axiomGroups(
      p,
      ['objectProperty'],
      [
        item({ type: 'TransitiveObjectProperty', property: p }),
        item({ type: 'ObjectPropertyDomain', property: p, domain: EX + 'A' }),
      ],
    );
    expect(g.map((x) => x.id)).toEqual(['domains', 'characteristics']);
    expect(g[1].items[0].word).toBe('Transitive');
  });

  it('writes a class’s axioms as a Manchester frame, with the rest left out', () => {
    const iri = EX + 'C';
    const groups = axiomGroups(
      iri,
      ['class'],
      [
        item({ type: 'SubClassOf', subClass: iri, superClass: EX + 'P' }),
        item({
          type: 'SubClassOf',
          subClass: iri,
          superClass: {
            type: 'ObjectSomeValuesFrom',
            property: EX + 'partOf',
            class: EX + 'Q',
          },
        }),
        item({ type: 'EquivalentClasses', classes: [iri, EX + 'E'] }),
        item({
          type: 'HasKey',
          class: iri,
          objectProperties: [EX + 'k1'],
          dataProperties: [EX + 'k2'],
        }),
        item({
          type: 'HasKey',
          class: iri,
          objectProperties: [EX + 'k3'],
          dataProperties: [],
        }),
        item({
          type: 'HasKey',
          class: EX + 'Other',
          objectProperties: [],
          dataProperties: [],
        }),
      ],
    );
    const name = (i: string) => (i === iri ? 'my class' : i.slice(EX.length));
    const f = manchesterFrame('Class', groups, name, {});
    expect(f.word).toBe('Class');
    expect(f.sections.map((s) => [s.word, s.items.length])).toEqual([
      ['SubClassOf', 2],
      ['EquivalentTo', 1],
      ['HasKey', 2],
    ]);
    expect(f.other).toHaveLength(1);
    expect(frameText(f, iri, name)).toBe(
      [
        "Class: 'my class'",
        '',
        '    SubClassOf: ',
        '        P,',
        '        partOf some Q',
        '',
        '    EquivalentTo: ',
        '        E',
        '',
        '    HasKey: ',
        '        k1, k2',
        '',
        '    HasKey: ',
        '        k3',
        '',
      ].join('\n'),
    );
  });

  it('writes a property’s characteristics and an individual’s facts in one clause each', () => {
    const p = EX + 'p';
    const pf = manchesterFrame(
      'ObjectProperty',
      axiomGroups(
        p,
        ['objectProperty'],
        [
          item({ type: 'InverseFunctionalObjectProperty', property: p }),
          item({ type: 'TransitiveObjectProperty', property: p }),
        ],
      ),
      (i) => i.slice(EX.length),
      {},
    );
    expect(pf.sections).toHaveLength(1);
    expect(pf.sections[0].word).toBe('Characteristics');
    expect(pf.sections[0].items.map((t) => t[0].text)).toEqual(['InverseFunctional', 'Transitive']);
    const a = EX + 'a';
    const af = manchesterFrame(
      'NamedIndividual',
      axiomGroups(
        a,
        ['individual'],
        [
          item({
            type: 'ObjectPropertyAssertion',
            property: EX + 'knows',
            source: a,
            target: EX + 'b',
          }),
          item({
            type: 'NegativeObjectPropertyAssertion',
            property: EX + 'knows',
            source: a,
            target: EX + 'c',
          }),
        ],
      ),
      (i) => i.slice(EX.length),
      {},
    );
    expect(af.word).toBe('Individual');
    expect(af.sections.map((s) => s.word)).toEqual(['Facts']);
    expect(frameText(af, a, (i) => i.slice(EX.length))).toContain('        not knows c\n');
  });

  it('keeps the inferences that the asserted axioms do not state', () => {
    const iri = EX + 'C';
    const groups = axiomGroups(
      iri,
      ['class'],
      [item({ type: 'SubClassOf', subClass: iri, superClass: EX + 'P' })],
    );
    const inf = inferredOf(
      iri,
      groups,
      { classes: [iri, EX + 'Same'], parentClasses: [EX + 'P', EX + 'T'] },
      true,
      false,
    );
    expect(inf.superClasses).toEqual([EX + 'T']);
    expect(inf.equivalents).toEqual([EX + 'Same']);
    const top = inferredOf(iri, [], { classes: [iri], parentClasses: [OWL_THING] }, true, true);
    expect(top.superClasses).toEqual([]);
  });

  it('groups the mentions by the restriction that holds the entity', () => {
    const iri = EX + 'Mito';
    const g = mentionGroups(iri, [
      item({
        type: 'SubClassOf',
        subClass: EX + 'Env',
        superClass: { type: 'ObjectSomeValuesFrom', property: EX + 'partOf', class: iri },
      }),
      item({
        type: 'SubClassOf',
        subClass: EX + 'Matrix',
        superClass: { type: 'ObjectSomeValuesFrom', property: EX + 'partOf', class: iri },
      }),
      item({
        type: 'EquivalentClasses',
        classes: [EX + 'X', { type: 'ObjectIntersectionOf', classes: [iri] }],
      }),
    ]);
    expect(g[0]).toMatchObject({
      property: EX + 'partOf',
      word: 'some',
      subjects: [EX + 'Env', EX + 'Matrix'],
    });
    expect(g[1]).toMatchObject({ property: null, subjects: [] });
    expect(g[1].axioms).toHaveLength(1);
  });
});

describe('the hierarchy', () => {
  it('follows the first parent by label from the entity to a root, and names its other parents', () => {
    const anc = {
      nodes: [node(EX + 'r', 'root', 2), node(EX + 'b', 'beta', 3), node(EX + 'a', 'alpha', 4)],
      edges: [
        [EX + 'x', EX + 'b'],
        [EX + 'x', EX + 'a'],
        [EX + 'a', EX + 'r'],
        [EX + 'b', EX + 'r'],
      ] as [string, string][],
    };
    const l = lineage(EX + 'x', anc);
    expect(l.spine.map((n) => n.label)).toEqual(['root', 'alpha']);
    expect(l.otherParents.map((n) => n.label)).toEqual(['beta']);
  });

  it('puts deprecated parents last and ends a path at a cycle', () => {
    const anc = {
      nodes: [node(EX + 'a', 'a', 1, true), node(EX + 'b', 'b'), node(EX + 'c', 'c')],
      edges: [
        [EX + 'x', EX + 'a'],
        [EX + 'x', EX + 'b'],
        [EX + 'b', EX + 'c'],
        [EX + 'c', EX + 'b'],
      ] as [string, string][],
    };
    const l = lineage(EX + 'x', anc);
    expect(l.spine.map((n) => n.label)).toEqual(['c', 'b']);
    expect(l.otherParents.map((n) => n.label)).toEqual(['a']);
    expect(lineage(EX + 'x', undefined)).toEqual({ spine: [], otherParents: [] });
  });

  it('chooses the tree of an entity from its kinds', () => {
    expect(treeKindOf(['individual', 'class'])).toBe('class');
    expect(treeKindOf(['concept'])).toBe('concept');
    expect(treeKindOf(['dataProperty'])).toBe('dataProperty');
    expect(treeKindOf(['individual'])).toBeNull();
    expect(kindWord(['annotationProperty'])).toBe('annotation property');
    expect(kindWord([])).toBe('resource');
  });
});

describe('the graph view', () => {
  const t: Pick<TermPage, 'iri' | 'label' | 'axioms' | 'mentions' | 'incoming' | 'names'> = {
    iri: EX + 'Mito',
    label: 'mitochondrion',
    axioms: [
      item({ type: 'SubClassOf', subClass: EX + 'Mito', superClass: EX + 'Organelle' }),
      item({
        type: 'SubClassOf',
        subClass: EX + 'Mito',
        superClass: { type: 'ObjectSomeValuesFrom', property: EX + 'partOf', class: EX + 'Cyto' },
      }),
    ],
    mentions: {
      axioms: [
        item({
          type: 'SubClassOf',
          subClass: EX + 'Env',
          superClass: { type: 'ObjectSomeValuesFrom', property: EX + 'partOf', class: EX + 'Mito' },
        }),
      ],
      total: 1,
      truncated: false,
    },
    incoming: [
      { property: RDFS + 'subClassOf', count: 2, more: false, subjects: [EX + 'K1', EX + 'K2'] },
    ],
    names: {
      [EX + 'partOf']: {
        label: 'part of',
        labelLang: null,
        shortForm: null,
        kinds: ['objectProperty'],
        deprecated: false,
      },
    },
  };

  it('draws the entity, its parents, restrictions, mentions and first children', () => {
    const g = neighbourhood(t, {}, 1);
    expect(g.nodes.find((n) => n.focus)?.label).toBe('mitochondrion');
    expect(
      g.edges.map((e) => [e.source.replace(EX, ''), e.label, e.target.replace(EX, '')]),
    ).toEqual([
      ['Mito', 'subclass of', 'Organelle'],
      ['Mito', 'part of', 'Cyto'],
      ['Env', 'part of', 'Mito'],
      ['K1', 'subclass of', 'Mito'],
    ]);
  });

  it('merges a node’s neighbourhood without moving the focus', () => {
    const a = neighbourhood(t, {});
    const b = {
      nodes: [
        { id: EX + 'Cyto', label: 'cytoplasm', focus: true },
        { id: EX + 'Cell', label: 'cell' },
      ],
      edges: [{ id: 'e', source: EX + 'Cyto', target: EX + 'Cell', label: 'part of' }],
    };
    const m = mergePieces(a, b);
    expect(m.nodes.filter((n) => n.focus).map((n) => n.id)).toEqual([EX + 'Mito']);
    expect(m.nodes.some((n) => n.id === EX + 'Cell')).toBe(true);
    expect(m.edges.some((e) => e.id === 'e')).toBe(true);
  });
});

describe('links', () => {
  it('builds the addresses of pages, downloads and editors', () => {
    expect(browseHref('/ui', 'go', EX + 'a#b', { graph: 'urn:g', branch: null })).toBe(
      '/ui/browse?ds=go&iri=http%3A%2F%2Fexample.org%2Fa%23b&graph=urn%3Ag',
    );
    expect(browseHref('/ui', 'go', null)).toBe('/ui/browse?ds=go');
    expect(downloadHref('go@dev', { graph: null }, EX + 'a', 'ttl')).toBe(
      '/$/ontology/go%40dev/term?graph=default&iri=http%3A%2F%2Fexample.org%2Fa&format=ttl',
    );
    expect(editHref('/ui', 'go', EX + 'p', ['objectProperty'], { graph: null })).toBe(
      '/ui/workbench/ontology?ds=go&iri=http%3A%2F%2Fexample.org%2Fp&kind=ObjectProperty',
    );
    expect(editHref('/ui', 'th', EX + 'c', ['concept'], { graph: 'urn:g' })).toBe(
      '/ui/workbench/thesaurus?ds=th&iri=http%3A%2F%2Fexample.org%2Fc&graph=urn%3Ag',
    );
  });

  it('names an IRI by its label, short form or prefixed name, and knows the built-in vocabularies', () => {
    const names = { [EX + 'a']: { label: 'Alpha' }, [EX + 'b']: { label: null, shortForm: 'B:1' } };
    expect(displayName(EX + 'a', names, {})).toBe('Alpha');
    expect(displayName(EX + 'b', names, {})).toBe('B:1');
    expect(displayName(EX + 'c', names, { ex: EX })).toBe('ex:c');
    expect(builtIn(OWL + 'Thing')).toBe(true);
    expect(builtIn(RDF + 'type')).toBe(true);
    expect(builtIn(EX + 'x')).toBe(false);
  });
});

describe('SPARQL of the landing page and the further reads', () => {
  it('reads the ontology header, preferring an ontology to a scheme', () => {
    const u = (value: string) => ({ type: 'uri', value });
    const l = (value: string, lang?: string) => ({
      type: 'literal',
      value,
      ...(lang ? { 'xml:lang': lang } : {}),
    });
    const rows = [
      { o: u(EX + 'scheme'), t: u(SKOS + 'ConceptScheme'), p: u(RDFS + 'label'), v: l('Scheme') },
      {
        o: u(EX + 'onto'),
        t: u(OWL + 'Ontology'),
        p: u('http://purl.org/dc/terms/title'),
        v: l('Titre', 'fr'),
      },
      {
        o: u(EX + 'onto'),
        t: u(OWL + 'Ontology'),
        p: u('http://purl.org/dc/terms/title'),
        v: l('Title', 'en'),
      },
      { o: u(EX + 'onto'), t: u(OWL + 'Ontology'), p: u(OWL + 'versionInfo'), v: l('1.2') },
      {
        o: u(EX + 'onto'),
        t: u(OWL + 'Ontology'),
        p: u('http://purl.org/dc/terms/creator'),
        v: l('Someone'),
      },
    ];
    const h = parseHead(rows, ['en']);
    expect(h?.scheme).toBe(false);
    expect(h?.head).toMatchObject({ iri: EX + 'onto', title: 'Title', version: '1.2' });
    expect(h?.head.annotations).toEqual([
      { property: 'http://purl.org/dc/terms/creator', value: { '@value': 'Someone' } },
    ]);
    expect(parseHead([], [])).toBeNull();
  });

  it('counts kinds in display order and leaves out empty ones', () => {
    expect(countsQuery('urn:g')).toContain('GRAPH <urn:g>');
    const rows = [
      { t: { type: 'uri', value: OWL + 'ObjectProperty' }, n: { type: 'literal', value: '9' } },
      { t: { type: 'literal', value: 'deprecated' }, n: { type: 'literal', value: '2' } },
      { t: { type: 'uri', value: OWL + 'Class' }, n: { type: 'literal', value: '10' } },
      { t: { type: 'uri', value: SKOS + 'Concept' }, n: { type: 'literal', value: '0' } },
    ];
    expect(parseCounts(rows).map((c) => [c.label, c.count])).toEqual([
      ['Classes', 10],
      ['of them deprecated', 2],
      ['Object properties', 9],
    ]);
  });

  it('reads where another dataset mentions an IRI', () => {
    const rows = [
      { g: { type: 'literal', value: '' }, n: { type: 'literal', value: '0' } },
      {
        g: { type: 'uri', value: 'urn:g' },
        n: { type: 'literal', value: '4' },
        label: { type: 'literal', value: 'x' },
      },
    ];
    expect(parseReuse('other', rows)).toEqual({
      dataset: 'other',
      graphs: [{ graph: 'urn:g', count: 4 }],
      label: 'x',
    });
    expect(parseReuse('none', [])).toBeNull();
  });

  it('picks the best definition of each root and pages incoming subjects', () => {
    expect(definitionsQuery([EX + 'a'], null)).toContain(`VALUES ?s { <${EX}a> }`);
    const rows = [
      {
        s: { type: 'uri', value: EX + 'a' },
        d: { type: 'literal', value: 'Définition', 'xml:lang': 'fr' },
      },
      { s: { type: 'uri', value: EX + 'a' }, d: { type: 'literal', value: 'Definition' } },
    ];
    expect(parseDefinitions(rows, ['en']).get(EX + 'a')).toBe('Definition');
    expect(incomingQuery(EX + 'a', RDF + 'type', ['urn:g'], 20)).toBe(
      `SELECT DISTINCT ?s WHERE { { VALUES ?g { <urn:g> } GRAPH ?g { ?s <${RDF}type> <${EX}a> FILTER(isIRI(?s)) } } } ORDER BY ?s LIMIT 50 OFFSET 20`,
    );
    // the default graph and the imported graphs the term answer names
    const q = incomingQuery(EX + 'a', RDF + 'type', ['default', 'urn:g', 'urn:imp'], 0);
    expect(q).toContain(
      `{ GRAPH <urn:x-arq:DefaultGraph> { ?s <${RDF}type> <${EX}a> FILTER(isIRI(?s)) } } UNION`,
    );
    expect(q).toContain('VALUES ?g { <urn:g> <urn:imp> }');
    expect(usageQuery(EX + 'p', ['default'], 50)).toBe(
      `SELECT DISTINCT ?s WHERE { { GRAPH <urn:x-arq:DefaultGraph> { ?s <${EX}p> ?o FILTER(isIRI(?s)) } } } ORDER BY ?s LIMIT 50 OFFSET 50`,
    );
  });
});

describe('namesQuery', () => {
  it('reads the labels in one graph, in the graphs a term answer read, or anywhere', () => {
    expect(namesQuery([EX + 'a'], 'urn:g')).toContain('GRAPH <urn:g> { ?s ');
    // a label may sit in a graph the one shown imports
    const q = namesQuery([EX + 'a'], 'urn:g', ['default', 'urn:g', 'urn:imp']);
    expect(q).toContain('VALUES ?g { <urn:g> <urn:imp> }');
    expect(q).toContain('UNION');
    expect(q).not.toContain('GRAPH <urn:g> { ?s ');
    expect(namesQuery([EX + 'a'], 'urn:g', ['default'])).not.toContain('GRAPH');
    expect(namesQuery([EX + 'a'], null)).not.toContain('GRAPH');
  });
});

describe('languageCount', () => {
  it('counts the distinct languages of translations, not their values', () => {
    expect(languageCount([{ lang: 'fr' }, { lang: 'fr' }, { lang: 'FR' }])).toBe(1);
    expect(languageCount([{ lang: 'fr' }, { lang: 'de' }, { lang: null }])).toBe(3);
    expect(languageCount([])).toBe(0);
  });
});

describe('hierarchyOf', () => {
  const X = 'http://x/';
  const node = (id: string, focus?: boolean) => ({ id, label: id, focus });
  const edge = (source: string, label: string, target: string) => ({
    id: `${source} ${label} ${target}`,
    source,
    target,
    label,
  });

  it('keeps the parents and children of what neighbourhood emits, and not the axioms', () => {
    const g = neighbourhood(
      {
        iri: X + 'c',
        axioms: [
          {
            axiom: { type: 'SubClassOf', subClass: X + 'c', superClass: X + 'parent' },
            ofn: '',
            graph: 'default',
          },
          {
            axiom: {
              type: 'SubClassOf',
              subClass: X + 'c',
              superClass: { type: 'ObjectSomeValuesFrom', property: X + 'p', class: X + 'filler' },
            },
            ofn: '',
            graph: 'default',
          },
        ],
        mentions: { axioms: [], total: 0, truncated: false },
        incoming: [
          {
            property: 'http://www.w3.org/2000/01/rdf-schema#subClassOf',
            count: 1,
            more: false,
            subjects: [X + 'kid'],
          },
        ],
        names: {},
      } as never,
      {},
    );
    expect(g.nodes.map((n) => n.id)).toContain(X + 'filler');
    const h = hierarchyOf(g);
    expect(h.nodes.map((n) => n.id).sort()).toEqual([X + 'c', X + 'kid', X + 'parent']);
    // a child's edge has the entity as its target, and a parent's as its source
    expect(h.edges.find((e) => e.source === X + 'kid')?.target).toBe(X + 'c');
    expect(h.edges.find((e) => e.target === X + 'parent')?.source).toBe(X + 'c');
  });

  it('keeps the first children and every parent', () => {
    const kids = Array.from({ length: 9 }, (_, i) => `k${i}`);
    const h = hierarchyOf(
      {
        nodes: [node('c', true), node('p1'), node('p2'), ...kids.map((k) => node(k))],
        edges: [
          ...kids.map((k) => edge(k, 'subclass of', 'c')),
          edge('c', 'subclass of', 'p1'),
          edge('c', 'subclass of', 'p2'),
        ],
      },
      6,
    );
    expect(h.edges.filter((e) => e.target === 'c')).toHaveLength(6);
    expect(h.edges.filter((e) => e.source === 'c')).toHaveLength(2);
    expect(h.nodes).toHaveLength(9);
    expect(h.nodes.map((n) => n.id)).not.toContain('k6');
  });

  it('reads broader and subproperty edges, and leaves out the neighbours of a neighbour', () => {
    const piece = {
      nodes: [node('c', true), node('p'), node('k'), node('sib'), node('far')],
      edges: [
        edge('c', 'broader', 'p'),
        edge('k', 'subproperty of', 'c'),
        // added by expanding the parent
        edge('sib', 'broader', 'p'),
        edge('p', 'broader', 'far'),
      ],
    };
    const h = hierarchyOf(piece);
    expect(h.nodes.map((n) => n.id).sort()).toEqual(['c', 'k', 'p']);
    expect(h.edges).toHaveLength(2);
  });

  it('keeps the entity alone when it has no hierarchy', () => {
    const h = hierarchyOf({
      nodes: [node('c', true), node('o')],
      edges: [edge('c', 'part of', 'o')],
    });
    expect(h.nodes.map((n) => n.id)).toEqual(['c']);
    expect(h.edges).toEqual([]);
  });
});

describe('the identifiers rows', () => {
  const x = (database: string, id: string) => ({
    database,
    id,
    href: null,
    value: `${database}:${id}`,
  });
  it('groups the references by database, in their order', () => {
    expect(
      xrefsByDatabase([x('FMA', '1'), x('PMID', '2'), x('PMID', '3'), x('Wikipedia', 'A')]).map(
        (r) => [r.database, r.ids.map((i) => i.id)],
      ),
    ).toEqual([
      ['FMA', ['1']],
      ['PMID', ['2', '3']],
      ['Wikipedia', ['A']],
    ]);
  });
  it('hides rows past the limit, unless only one would hide', () => {
    expect(hiddenRows(3, 8)).toBe(0);
    expect(hiddenRows(8, 8)).toBe(0);
    expect(hiddenRows(9, 8)).toBe(0);
    expect(hiddenRows(10, 8)).toBe(2);
    expect(hiddenRows(14, 8)).toBe(6);
  });
});
