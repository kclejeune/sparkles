import { describe, expect, it } from 'vitest';
import type { SupportFact } from './api';
import { patchTerm, removalPatch } from './support-patch';

const id = '9d0c41e2-0000-4000-8000-000000000000';
const fact: SupportFact = {
  triple: [
    { type: 'bnode', value: 'b80000000003' },
    { type: 'uri', value: 'urn:p' },
    { type: 'literal', value: 'kept', 'xml:lang': 'en', 'its:dir': 'ltr' },
  ],
  graph: { type: 'uri', value: 'urn:graph' },
  role: 'assertion',
};

describe('support removals as RDF Patch', () => {
  it('pins the draft branch commit and preserves inherited stored blank labels', () => {
    expect(removalPatch(fact, id, 42)).toBe(
      `H prev <urn:uuid:${id}#commit:42> .\nTX .\nD <_:b80000000003> <urn:p> "kept"@en--ltr <urn:graph> .\nTC .\n`,
    );
  });
  it('cannot turn literal or IRI data into another patch operation', () => {
    const value: SupportFact = {
      ...fact,
      triple: [
        fact.triple[0],
        { type: 'uri', value: 'urn:p>\nA <urn:attacker>' },
        { type: 'literal', value: '"\nA <urn:attacker> <urn:p> <urn:o> .\n' },
      ],
    };
    const patch = removalPatch(value, id, 42);
    expect(patch.split('\n')).toHaveLength(5);
    expect(patch).not.toMatch(/^A /m);
    expect(patch).toContain('\\u003e\\u000a');
    expect(patch).toContain('\\"\\nA');
  });
  it('preserves nested RDF triple terms and rejects invalid commit identities', () => {
    expect(
      patchTerm({
        type: 'triple',
        value: {
          subject: fact.triple[0],
          predicate: fact.triple[1],
          object: {
            type: 'literal',
            value: '01',
            datatype: 'http://www.w3.org/2001/XMLSchema#integer',
          },
        },
      }),
    ).toBe('<<( <_:b80000000003> <urn:p> "01"^^<http://www.w3.org/2001/XMLSchema#integer> )>>');
    expect(() => removalPatch(fact, 'bad\nTX .', 42)).toThrow('commit identity');
    expect(() => removalPatch(fact, id, Number.MAX_SAFE_INTEGER + 1)).toThrow('commit identity');
  });
});
