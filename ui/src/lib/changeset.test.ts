import { describe, expect, it } from 'vitest';
import { changeGroups, changeIris, parseNt } from './changeset';

const EX = 'http://example.org/';

describe('change sets', () => {
  it('reads N-Triples terms', () => {
    expect(parseNt(`<${EX}a>`)).toEqual({ type: 'iri', value: EX + 'a' });
    expect(parseNt('_:b1')).toEqual({ type: 'bnode', value: 'b1' });
    expect(parseNt('"water"@en')).toEqual({ type: 'literal', value: 'water', lang: 'en' });
    expect(parseNt('"say \\"hi\\""')).toEqual({ type: 'literal', value: 'say "hi"' });
    expect(parseNt(`"3"^^<http://www.w3.org/2001/XMLSchema#integer>`)).toEqual({
      type: 'literal',
      value: '3',
      datatype: 'http://www.w3.org/2001/XMLSchema#integer',
    });
  });

  it('closes a literal at the first unescaped quote, even after an escaped backslash', () => {
    expect(parseNt('"C:\\\\"@en')).toEqual({ type: 'literal', value: 'C:\\', lang: 'en' });
    expect(parseNt('"a\\\\\\"b"')).toEqual({ type: 'literal', value: 'a\\"b' });
    // a quote inside the value is not the end, and a language tag that holds a quote is not read
    expect(parseNt('"x\\"y"^^<http://www.w3.org/2001/XMLSchema#string>')).toEqual({
      type: 'literal',
      value: 'x"y',
      datatype: 'http://www.w3.org/2001/XMLSchema#string',
    });
  });

  it('groups changes by subject, removals before additions of a property', () => {
    const gs = changeGroups([
      { op: '+', subject: `<${EX}a>`, predicate: `<${EX}label>`, object: '"new"', graph: null },
      { op: '-', subject: `<${EX}a>`, predicate: `<${EX}label>`, object: '"old"', graph: null },
      { op: '+', subject: `<${EX}b>`, predicate: `<${EX}p>`, object: `<${EX}a>`, graph: null },
    ]);
    expect(gs.map((g) => g.subject.value)).toEqual([EX + 'a', EX + 'b']);
    expect(gs[0].changes.map((c) => c.op)).toEqual(['-', '+']);
    expect([gs[0].added, gs[0].removed]).toEqual([1, 1]);
    expect(
      changeIris([
        { op: '+', subject: `<${EX}b>`, predicate: `<${EX}p>`, object: `<${EX}a>`, graph: null },
      ]),
    ).toEqual([EX + 'b', EX + 'p', EX + 'a']);
  });
});
