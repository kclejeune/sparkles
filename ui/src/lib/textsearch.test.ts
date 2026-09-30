import { describe, expect, it } from 'vitest';
import { ApiError } from './api';
import {
  buildTextQuery,
  DEFAULT_GRAPH,
  escapeTextQuery,
  parsePredicateList,
  sparqlLiteral,
  textErrorHint,
  textHits,
} from './textsearch';

describe('sparqlLiteral', () => {
  it('escapes quotes, backslashes and control characters', () => {
    expect(sparqlLiteral('plain')).toBe('"plain"');
    expect(sparqlLiteral('say "hi"')).toBe('"say \\"hi\\""');
    expect(sparqlLiteral('a\\:b')).toBe('"a\\\\:b"');
    expect(sparqlLiteral('line\nnext\ttab\r')).toBe('"line\\nnext\\ttab\\r"');
  });

  it('round-trips through a JSON-compatible reading of the literal', () => {
    // SPARQL's ECHAR escapes used here are a subset of JSON's
    for (const s of ['"quick bro"*', 'a \\ b', '+fox -dog', 'naïve café', '10\\:30'])
      expect(JSON.parse(sparqlLiteral(s))).toBe(s);
  });
});

describe('escapeTextQuery', () => {
  it('escapes bare colons and keeps the rest of the syntax', () => {
    expect(escapeTextQuery('10:30')).toBe('10\\:30');
    expect(escapeTextQuery('http://example.org/x')).toBe('http\\://example.org/x');
    expect(escapeTextQuery('"brown fox"~2 AND +quick -lazy (a OR b)')).toBe(
      '"brown fox"~2 AND +quick -lazy (a OR b)',
    );
    expect(escapeTextQuery('"quick bro"*')).toBe('"quick bro"*');
  });

  it('does not double an escape the user already wrote', () => {
    expect(escapeTextQuery('10\\:30')).toBe('10\\:30');
    expect(escapeTextQuery('a\\"b')).toBe('a\\"b');
  });
});

describe('buildTextQuery', () => {
  it('builds a ranked query with every result slot', () => {
    const q = buildTextQuery('brown fox', { limit: 20 });
    expect(q).toContain('PREFIX text: <http://jena.apache.org/text#>');
    expect(q).toContain('(?s ?score ?literal ?graph ?predicate) text:query ("brown fox" 20) .');
    expect(q).toContain('ORDER BY DESC(?score)');
    expect(q).toContain('LIMIT 20');
  });

  it('adds predicates and a language, escaping the query string', () => {
    const q = buildTextQuery(' "say \\"hi\\"" at 10:30 ', {
      predicates: ['http://www.w3.org/2000/01/rdf-schema#label'],
      lang: 'en',
      limit: 5,
    });
    expect(q).toContain(
      'text:query (<http://www.w3.org/2000/01/rdf-schema#label> "\\"say \\\\\\"hi\\\\\\"\\" at 10\\\\:30" 5 "lang:en")',
    );
  });

  it('searches the named graphs too on request', () => {
    const q = buildTextQuery('fox', { limit: 5, namedGraphs: true });
    const call = '(?s ?score ?literal ?graph ?predicate) text:query ("fox" 5) .';
    expect(q).toContain(`{ ${call} }\n  UNION\n  { GRAPH ?g { ${call} } }`);
    expect(q).toContain('LIMIT 5');
  });

  it('keeps the limit positive and whole', () => {
    expect(buildTextQuery('x', { limit: 0 })).toContain('("x" 1)');
    expect(buildTextQuery('x', { limit: 7.9 })).toContain('("x" 7)');
  });
});

describe('textHits', () => {
  it('shapes rows, hides the default graph and sorts by score', () => {
    const hits = textHits([
      {
        s: { type: 'uri', value: 'http://ex/a' },
        score: { type: 'literal', value: '1.5' },
        literal: { type: 'literal', value: 'a fox' },
        graph: { type: 'uri', value: DEFAULT_GRAPH },
        predicate: { type: 'uri', value: 'http://ex/label' },
      },
      {
        s: { type: 'uri', value: 'http://ex/b' },
        score: { type: 'literal', value: '3.25' },
        graph: { type: 'uri', value: 'http://ex/g' },
      },
      { score: { type: 'literal', value: '9' } },
    ]);
    expect(hits.map((h) => (h.subject.type === 'uri' ? h.subject.value : ''))).toEqual([
      'http://ex/b',
      'http://ex/a',
    ]);
    expect(hits[0].graph).toBe('http://ex/g');
    expect(hits[1].graph).toBeUndefined();
    expect(hits[1].predicate).toBe('http://ex/label');
    expect(hits[1].score).toBe(1.5);
  });
});

describe('parsePredicateList', () => {
  const prefixes = { rdfs: 'http://www.w3.org/2000/01/rdf-schema#', '': 'http://ex/' };

  it('accepts IRIs, <IRIs> and prefixed names, one per line or comma separated', () => {
    const r = parsePredicateList(
      'rdfs:label\n<http://purl.org/dc/terms/title>, http://schema.org/name\n:local\n\nrdfs:label',
      prefixes,
    );
    expect(r.iris).toEqual([
      'http://www.w3.org/2000/01/rdf-schema#label',
      'http://purl.org/dc/terms/title',
      'http://schema.org/name',
      'http://ex/local',
    ]);
    expect(r.bad).toEqual([]);
  });

  it('reports entries that are not IRIs', () => {
    expect(parsePredicateList('label unknown:x <relative>', prefixes).bad).toEqual([
      'label',
      '<relative>',
    ]);
  });
});

describe('textErrorHint', () => {
  it('explains a missing index, a bad query and a rebuild', () => {
    const noIndex = textErrorHint(
      new ApiError(400, 'dataset has no full-text index; enable it with `sparkles text-index`'),
    );
    expect(noIndex.hint).toContain('Full-text search panel');
    const syntax = textErrorHint(new ApiError(400, "text:query: Syntax Error: 'a AND'"));
    expect(syntax.hint).toContain('query syntax');
    const busy = textErrorHint(new ApiError(503, 'full-text index of x is stale'));
    expect(busy.title).toContain('being rebuilt');
    expect(textErrorHint(new ApiError(501, 'built without')).title).toContain('without');
  });
});
