import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import {
  addMissingPrefixes,
  applyMissingPrefixes,
  displayTerm,
  isUpdate,
  localName,
  missingPrefixDecls,
  queryKind,
  shorten,
  sparqlIri,
  stripQueryNoise,
  termKey,
  toSparql,
  usedPrefixes,
  WELL_KNOWN,
} from './rdf';

const ex = (l: string): Term => ({ type: 'uri', value: `http://ex.org/${l}` });
const triple = (s: Term, p: Term, o: Term): Term => ({
  type: 'triple',
  value: { subject: s, predicate: p, object: o },
});

describe('toSparql', () => {
  it('writes IRIs, blank nodes and literals', () => {
    expect(toSparql(ex('a'))).toBe('<http://ex.org/a>');
    expect(toSparql({ type: 'bnode', value: 'b0' })).toBe('_:b0');
    expect(toSparql({ type: 'literal', value: 'x' })).toBe('"x"');
    expect(toSparql({ type: 'literal', value: 'x', 'xml:lang': 'en' })).toBe('"x"@en');
    expect(toSparql({ type: 'literal', value: '1', datatype: WELL_KNOWN.xsd + 'integer' })).toBe(
      '"1"^^<http://www.w3.org/2001/XMLSchema#integer>',
    );
  });

  it('omits xsd:string and escapes quotes and newlines', () => {
    expect(
      toSparql({ type: 'literal', value: 'a"b\nc', datatype: WELL_KNOWN.xsd + 'string' }),
    ).toBe('"a\\"b\\nc"');
  });

  // RDF 1.2: `<<( s p o )>>` is a triple term; bare `<< s p o >>` is reification syntax.
  it('writes triple terms in RDF 1.2 syntax, nested too', () => {
    const inner = triple(ex('a'), ex('p'), { type: 'literal', value: '1' });
    expect(toSparql(inner)).toBe('<<( <http://ex.org/a> <http://ex.org/p> "1" )>>');
    expect(toSparql(triple(inner, ex('q'), ex('b')))).toBe(
      '<<( <<( <http://ex.org/a> <http://ex.org/p> "1" )>> <http://ex.org/q> <http://ex.org/b> )>>',
    );
  });

  it('gives distinct keys to terms that differ only in kind', () => {
    const keys = new Set(
      [
        ex('a'),
        { type: 'literal', value: 'http://ex.org/a' } as Term,
        { type: 'bnode', value: 'a' } as Term,
        { type: 'literal', value: 'a', 'xml:lang': 'en' } as Term,
        { type: 'literal', value: 'a' } as Term,
      ].map(termKey),
    );
    expect(keys.size).toBe(5);
  });
});

describe('displayTerm', () => {
  it('shortens IRIs, including inside triple terms', () => {
    const t = triple(
      { type: 'uri', value: WELL_KNOWN.rdf + 'type' },
      { type: 'uri', value: WELL_KNOWN.rdfs + 'label' },
      { type: 'literal', value: 'x' },
    );
    expect(displayTerm(t, WELL_KNOWN)).toBe('<< rdf:type rdfs:label x >>');
    expect(displayTerm(null, WELL_KNOWN)).toBe('');
  });
});

describe('shorten / localName / sparqlIri', () => {
  it('prefers the longest matching namespace', () => {
    const pm = { a: 'http://ex.org/', b: 'http://ex.org/sub/' };
    expect(shorten('http://ex.org/sub/x', pm)).toBe('b:x');
    expect(shorten('http://ex.org/x', pm)).toBe('a:x');
    expect(shorten('http://other.org/x', pm)).toBeNull();
  });

  it('refuses local parts that are not valid prefixed names', () => {
    expect(shorten('http://ex.org/a b', { a: 'http://ex.org/' })).toBeNull();
    expect(shorten('http://ex.org/x.', { a: 'http://ex.org/' })).toBeNull();
    expect(shorten('http://ex.org/', { a: 'http://ex.org/' })).toBe('a:');
  });

  it('takes the last path or fragment segment', () => {
    expect(localName('http://ex.org/ns#Person')).toBe('Person');
    expect(localName('http://ex.org/a/b/')).toBe('b');
    expect(localName('http://ex.org/caf%C3%A9')).toBe('café');
    expect(localName('http://ex.org/%E0%A4%A')).toBe('%E0%A4%A');
  });

  it('percent-encodes characters that cannot appear in an IRIREF', () => {
    expect(sparqlIri('http://ex.org/a b>')).toBe('<http://ex.org/a%20b%3E>');
  });
});

describe('query text analysis', () => {
  it('ignores keywords in strings, IRIs and comments', () => {
    expect(stripQueryNoise('SELECT * { ?s ?p "DELETE" } # INSERT')).toBe('SELECT * { ?s ?p "" } ');
    expect(isUpdate('SELECT * WHERE { ?s <http://ex.org/INSERT> ?o }')).toBe(false);
    expect(isUpdate('# DELETE everything\nASK {}')).toBe(false);
  });

  it('classifies updates and queries', () => {
    expect(isUpdate('INSERT DATA { <a> <b> <c> }')).toBe(true);
    expect(isUpdate('INSERT { ?s a ?c } WHERE { SELECT ?s ?c { ?s a ?c } }')).toBe(true);
    expect(isUpdate('SELECT * { ?s ?p ?o FILTER NOT EXISTS { ?s a ?c } }')).toBe(false);
    expect(queryKind('PREFIX ex: <http://ex.org/>\nconstruct { } where { }')).toBe('CONSTRUCT');
    expect(queryKind('CLEAR ALL')).toBe('UPDATE');
    expect(queryKind('')).toBeNull();
  });

  it('finds used prefixes but not variables, IRIs or strings', () => {
    const used = usedPrefixes('SELECT ?x { ?x foaf:name "a:b" ; <http://x/y:z> :local }');
    expect([...used].sort()).toEqual(['', 'foaf']);
  });

  it('adds only missing, known prefixes', () => {
    const q =
      'PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\nSELECT * { ?s rdf:type owl:Class ; foo:bar ?o }';
    expect(missingPrefixDecls(q, WELL_KNOWN)).toEqual([
      'PREFIX owl: <http://www.w3.org/2002/07/owl#>',
    ]);
    expect(addMissingPrefixes('ASK {}', WELL_KNOWN)).toBe('ASK {}');
  });
});

describe('applyMissingPrefixes', () => {
  const q = 'SELECT * { ?s a owl:Class }';
  const fixed = 'PREFIX owl: <http://www.w3.org/2002/07/owl#>\n' + q;

  it('fixes the captured text and writes it back to an unchanged tab', () => {
    const tab = { query: q };
    expect(applyMissingPrefixes(tab, q, WELL_KNOWN)).toBe(fixed);
    expect(tab.query).toBe(fixed);
  });

  // Run captures the tab's text before awaiting the prefix map; the user may type meanwhile.
  it('runs the captured text but keeps edits made while waiting', () => {
    const tab = { query: q };
    const captured = tab.query;
    tab.query = q + ' LIMIT 5';
    expect(applyMissingPrefixes(tab, captured, WELL_KNOWN)).toBe(fixed);
    expect(tab.query).toBe(q + ' LIMIT 5');
  });

  it('leaves complete queries alone', () => {
    const tab = { query: 'ASK {}' };
    expect(applyMissingPrefixes(tab, 'ASK {}', WELL_KNOWN)).toBe('ASK {}');
    expect(tab.query).toBe('ASK {}');
  });
});
