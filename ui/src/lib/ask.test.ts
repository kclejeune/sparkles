import { afterEach, describe, expect, it, vi } from 'vitest';
import type { CheckResult, CheckTerm, Diagnosis } from './ask-api';
import {
  addAsked,
  applyProposals,
  ASKED_MAX,
  askedKey,
  checkedLine,
  emptyExplanation,
  EMPTY_HEADLINE,
  hasQuestion,
  iriSpans,
  isEmptyResult,
  isNotAQuery,
  loadAsked,
  lowerCamel,
  nameFromQuestion,
  proposeParameters,
  questionTitle,
  rememberAsked,
  termLine,
  type Asked,
} from './ask';

const P = {
  ex: 'http://example.org/ontology#',
  res: 'http://example.org/resource/',
};

const term = (t: Partial<CheckTerm> & Pick<CheckTerm, 'term' | 'iri' | 'kind'>): CheckTerm => ({
  occurs: true,
  ...t,
});

describe('the terms list', () => {
  it('describes properties, classes and entities', () => {
    expect(
      termLine(
        term({
          term: 'ex:memberOf',
          iri: P.ex + 'memberOf',
          kind: 'property',
          label: 'member of',
          count: 214,
        }),
      ),
    ).toBe('ex:memberOf "member of" · property · 214 triples');
    expect(termLine(term({ term: 'ex:Team', iri: P.ex + 'Team', kind: 'class', count: 1 }))).toBe(
      'ex:Team · class · 1 instance',
    );
    expect(
      termLine(
        term({
          term: 'res:payments',
          iri: P.res + 'payments',
          kind: 'entity',
          label: 'Payments team',
          types: ['ex:Team'],
        }),
      ),
    ).toBe('res:payments "Payments team" · entity · ex:Team');
    expect(
      termLine(term({ term: 'ex:nope', iri: P.ex + 'nope', kind: 'entity', occurs: false })),
    ).toBe('ex:nope · entity · not in the data you can read');
  });

  it('sums up a check', () => {
    const r: CheckResult = {
      dataset: 'org',
      ok: true,
      issues: [],
      estimatedRows: 12,
      prefixes: {},
    };
    expect(checkedLine(r)).toBe('no issues · estimated 12 rows');
    expect(
      checkedLine({
        ...r,
        ok: false,
        estimatedRows: undefined,
        issues: [
          { code: 'unknown-term', severity: 'error', message: 'x' },
          { code: 'language-tag', severity: 'warning', message: 'y' },
          { code: 'language-tag', severity: 'warning', message: 'z' },
        ],
      }),
    ).toBe('1 error · 2 warnings');
    expect(
      isNotAQuery({ ...r, issues: [{ code: 'not-a-query', severity: 'error', message: '' }] }),
    ).toBe(true);
    expect(isNotAQuery(r)).toBe(false);
  });

  it('knows when a tab has a question', () => {
    expect(hasQuestion(undefined)).toBe(false);
    expect(hasQuestion({ original: 'ASK {}' })).toBe(false);
    expect(hasQuestion({ original: 'ASK {}', question: 'Is it?' })).toBe(true);
    expect(questionTitle('Who is on the payments team and since when exactly?')).toBe(
      'Who is on the payments team and…',
    );
  });
});

describe('Save as example', () => {
  const query = `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
SELECT ?person WHERE {
  ?person ex:memberOf res:payments .
  # res:payments in a comment stays
  FILTER(?person != <http://example.org/resource/payments>)
  BIND("res:payments" AS ?text)
}`;
  const terms: CheckTerm[] = [
    term({ term: 'ex:memberOf', iri: P.ex + 'memberOf', kind: 'property', count: 214 }),
    term({
      term: 'res:payments',
      iri: P.res + 'payments',
      kind: 'entity',
      label: 'Payments team',
      types: ['ex:Team'],
    }),
    term({ term: 'res:untyped', iri: P.res + 'untyped', kind: 'entity' }),
  ];

  it('names parameters after the type in lower camel case', () => {
    expect(lowerCamel('Team')).toBe('team');
    expect(lowerCamel('OrganizationalUnit')).toBe('organizationalUnit');
    expect(lowerCamel('HTTPServer')).toBe('httpServer');
    expect(lowerCamel('team-member')).toBe('teamMember');
    expect(lowerCamel('3D')).toBe('p3D');
  });

  it('proposes a parameter per typed entity', () => {
    expect(proposeParameters(query, terms, P)).toEqual([
      {
        name: 'team',
        iri: P.res + 'payments',
        term: 'res:payments',
        label: 'Payments team',
        type: 'ex:Team',
      },
    ]);
  });

  it('makes names unique with a numeric suffix', () => {
    const two = [
      ...terms,
      term({ term: 'res:checkout', iri: P.res + 'checkout', kind: 'entity', types: ['ex:Team'] }),
    ];
    const names = proposeParameters('SELECT ?team WHERE { ?team ?p ?o }', two, P).map(
      (p) => p.name,
    );
    expect(names).toEqual(['team2', 'team3']);
  });

  it('replaces the constant everywhere it is used, and nowhere else', () => {
    const [p] = proposeParameters(query, terms, P);
    const out = applyProposals(query, [p], P);
    expect(out).toContain('?person ex:memberOf ?team .');
    expect(out).toContain('FILTER(?person != ?team)');
    expect(out).toContain('# res:payments in a comment stays');
    expect(out).toContain('BIND("res:payments" AS ?text)');
    expect(out).toContain('PREFIX res: <http://example.org/resource/>');
    expect(applyProposals(query, [], P)).toBe(query);
  });

  it('finds IRIs written with the dataset prefixes or with the query own', () => {
    const spans = iriSpans('SELECT * { res:a ex:p <urn:x> . ?s a ex:T . }', P).map((s) => s.iri);
    expect(spans).toEqual([P.res + 'a', P.ex + 'p', 'urn:x', P.ex + 'T']);
    const own = iriSpans('PREFIX r: <http://r/>\nASK { r:x.y r:p "v" . }', {}).map((s) => s.iri);
    expect(own).toEqual(['http://r/x.y', 'http://r/p']);
  });

  it('names the stored query after the question', () => {
    expect(nameFromQuestion('Who is on the payments team?')).toBe('who-is-on-the-payments-team');
    expect(nameFromQuestion('¿Qué?')).toBe('que');
    expect(nameFromQuestion('???')).toBe('example');
    expect(nameFromQuestion('x'.repeat(100)).length).toBe(64);
  });
});

describe('the Asked history', () => {
  const entry = (i: number): Asked => ({
    question: `Q${i}`,
    query: `ASK { ${i} }`,
    at: new Date(2026, 0, 1, 0, i).toISOString(),
  });

  it('puts the newest first and keeps a question once', () => {
    let list: Asked[] = [];
    list = addAsked(list, entry(1));
    list = addAsked(list, entry(2));
    list = addAsked(list, { ...entry(1), at: 'later' });
    expect(list.map((a) => a.question)).toEqual(['Q1', 'Q2']);
    expect(list[0].at).toBe('later');
  });

  it('keeps the last 50', () => {
    let list: Asked[] = [];
    for (let i = 0; i < 60; i++) list = addAsked(list, entry(i));
    expect(list).toHaveLength(ASKED_MAX);
    expect(list[0].question).toBe('Q59');
    expect(list.at(-1)?.question).toBe('Q10');
  });

  describe('in storage', () => {
    afterEach(() => vi.unstubAllGlobals());

    it('is kept per dataset and principal', () => {
      const store = new Map<string, string>();
      vi.stubGlobal('localStorage', {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => store.set(k, v),
        removeItem: (k: string) => store.delete(k),
      });
      expect(askedKey('org', 'ana')).toBe('sparkles.asked.ana.org');
      expect(askedKey('org', null)).toBe('sparkles.asked.local.org');
      rememberAsked('org', 'ana', entry(1));
      rememberAsked('org', 'ana', entry(2));
      rememberAsked('org', 'kai', entry(3));
      expect(loadAsked('org', 'ana').map((a) => a.question)).toEqual(['Q2', 'Q1']);
      expect(loadAsked('org', 'kai').map((a) => a.question)).toEqual(['Q3']);
      expect(loadAsked('other', 'ana')).toEqual([]);
      store.set('sparkles.asked.ana.bad', '{"not":"a list"}');
      expect(loadAsked('bad', 'ana')).toEqual([]);
    });
  });
});

describe('an empty result', () => {
  const d: Diagnosis = {
    dataset: 'org',
    empty: true,
    first: {
      kind: 'pattern',
      text: '?p foaf:name "Ana Lima"',
      constants: [
        { term: 'foaf:name', occurs: true },
        { term: '"Ana Lima"', occurs: false },
      ],
      issues: [
        {
          code: 'language-tag',
          severity: 'warning',
          message: 'The names are tagged @en.',
        },
      ],
    },
    steps: [],
    complete: true,
    message: 'The pattern ?p foaf:name "Ana Lima" has no solutions.',
    prefixes: {},
  };

  it('says what matched nothing', () => {
    expect(emptyExplanation(d)).toEqual({
      headline: EMPTY_HEADLINE,
      message: d.message,
      first: {
        kind: 'pattern',
        text: '?p foaf:name "Ana Lima"',
        missing: ['"Ana Lima"'],
        issues: ['The names are tagged @en.'],
      },
    });
    expect(EMPTY_HEADLINE).toBe('No data you can read matched.');
  });

  it('says nothing when there is nothing to diagnose', () => {
    expect(emptyExplanation({ ...d, empty: false })).toBeNull();
    expect(emptyExplanation(null)).toBeNull();
    expect(emptyExplanation({ ...d, first: undefined })).toEqual({
      headline: EMPTY_HEADLINE,
      message: d.message,
    });
  });

  it('knows an empty result', () => {
    expect(isEmptyResult({ queryType: 'SELECT', rows: [] })).toBe(true);
    expect(isEmptyResult({ queryType: 'SELECT', rows: [[]] })).toBe(false);
    expect(isEmptyResult({ queryType: 'ASK', boolean: false })).toBe(true);
    expect(isEmptyResult({ queryType: 'ASK', boolean: true })).toBe(false);
    expect(isEmptyResult({ queryType: 'CONSTRUCT' })).toBe(false);
  });
});
