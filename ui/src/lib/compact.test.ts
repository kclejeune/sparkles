import { describe, expect, it } from 'vitest';
import { compactIri, localName, parseCompact } from './compact';

const P = { ex: 'http://example.org/', xsd: 'http://www.w3.org/2001/XMLSchema#' };

describe('parseCompact', () => {
  it('reads IRIs and prefixed names', () => {
    expect(parseCompact('ex:ana', P)).toEqual({ type: 'uri', value: 'http://example.org/ana' });
    expect(parseCompact('<urn:uuid:1>', P)).toEqual({ type: 'uri', value: 'urn:uuid:1' });
    expect(compactIri('ex:a', P)).toBe('http://example.org/a');
    expect(compactIri('"x"', P)).toBeNull();
  });

  it('keeps an unknown prefix as written', () => {
    expect(parseCompact('zz:a', P)).toEqual({ type: 'uri', value: 'zz:a' });
  });

  it('reads literals', () => {
    expect(parseCompact('"Ana Lima"@en', P)).toEqual({
      type: 'literal',
      value: 'Ana Lima',
      'xml:lang': 'en',
    });
    expect(parseCompact('"5"^^xsd:integer', P)).toEqual({
      type: 'literal',
      value: '5',
      datatype: 'http://www.w3.org/2001/XMLSchema#integer',
    });
    expect(parseCompact('"a \\"b\\"\\n"', P)).toEqual({ type: 'literal', value: 'a "b"\n' });
    expect(parseCompact('42', P)).toMatchObject({ value: '42', datatype: P.xsd + 'integer' });
    expect(parseCompact('0.9', P)).toMatchObject({ datatype: P.xsd + 'decimal' });
    expect(parseCompact('true', P)).toMatchObject({ datatype: P.xsd + 'boolean' });
  });

  it('reads blank nodes and triple terms', () => {
    expect(parseCompact('_:b0', P)).toEqual({ type: 'bnode', value: 'b0' });
    expect(parseCompact('<<( ex:a ex:p "x" )>>', P)).toEqual({
      type: 'triple',
      value: {
        subject: { type: 'uri', value: 'http://example.org/a' },
        predicate: { type: 'uri', value: 'http://example.org/p' },
        object: { type: 'literal', value: 'x' },
      },
    });
  });

  it('shows text it cannot read as a literal', () => {
    expect(parseCompact('not a term', P)).toEqual({ type: 'literal', value: 'not a term' });
  });
});

describe('localName', () => {
  it('takes what follows the last separator', () => {
    expect(localName('http://example.org/ontology#Team')).toBe('Team');
    expect(localName('http://example.org/Team')).toBe('Team');
    expect(localName('urn:x:Team')).toBe('Team');
  });
});
