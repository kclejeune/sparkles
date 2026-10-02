import { describe, expect, it } from 'vitest';
import {
  readShaclSyntax,
  shaclSyntaxKey,
  shapesLabel,
  shapesMediaType,
  shapesPrefixes,
} from './shacl';

describe('shapes syntax', () => {
  it('reads the stored syntax, Turtle by default', () => {
    expect(readShaclSyntax('shaclc')).toBe('shaclc');
    expect(readShaclSyntax('turtle')).toBe('turtle');
    expect(readShaclSyntax(null)).toBe('turtle');
    expect(readShaclSyntax(42)).toBe('turtle');
    expect(shaclSyntaxKey('ds')).toBe('sparkles.shacl.syntax.ds');
  });

  it('sends SHACLC as text/shaclc', () => {
    expect(shapesMediaType('shaclc')).toBe('text/shaclc');
    expect(shapesMediaType('turtle')).toBe('text/turtle');
    expect(shapesLabel('shaclc')).toContain('SHACLC');
  });

  it('collects the declared prefixes of both syntaxes', () => {
    const ttl = '@prefix ex: <http://ex.org/> .\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\n';
    expect(shapesPrefixes(ttl, 'turtle')).toEqual({
      ex: 'http://ex.org/',
      foaf: 'http://xmlns.com/foaf/0.1/',
    });
    const c = shapesPrefixes('prefix : <http://ex.org/>\nshape :S { }', 'shaclc');
    expect(c['']).toBe('http://ex.org/');
    expect(c.sh).toBe('http://www.w3.org/ns/shacl#');
  });
});
