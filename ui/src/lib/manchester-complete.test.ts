import { describe, expect, it } from 'vitest';
import { completionWord, manchesterCompletions, type NameOption } from './manchester-complete';

const NAMES: NameOption[] = [
  { name: 'Graduate student', iri: 'http://ex.org/GraduateStudent', kind: 'class' },
  { name: 'uni:GraduateStudent', iri: 'http://ex.org/GraduateStudent', kind: 'class' },
  { name: 'Student', iri: 'http://ex.org/Student', kind: 'class' },
  { name: 'enrolled in', iri: 'http://ex.org/enrolledIn', kind: 'object property' },
  { name: 'some thing', iri: 'http://ex.org/someThing', kind: 'class' },
];

describe('the word being completed', () => {
  it('is the bare word before the cursor', () => {
    expect(completionWord('Person and Stu')).toBe('Stu');
    expect(completionWord('(enrolled')).toBe('enrolled');
    expect(completionWord('Person and ')).toBeNull();
  });

  it('is an open quoted name, with its quote', () => {
    expect(completionWord("Person and 'Grad")).toBe("'Grad");
    expect(completionWord("'it\\'s")).toBe("'it\\'s");
    expect(completionWord("'Graduate student' and ")).toBeNull();
  });

  it('skips a literal', () => {
    expect(completionWord('name value "a b" and St')).toBe('St');
    expect(completionWord('name value "a b')).toBeNull();
  });
});

describe('completions', () => {
  it('put prefix matches first and quote names that need it', () => {
    const c = manchesterCompletions('stu', NAMES);
    expect(c.map((x) => x.label)).toEqual(['Student', 'Graduate student', 'uni:GraduateStudent']);
    expect(c.find((x) => x.label === 'Graduate student')?.apply).toBe("'Graduate student'");
  });

  it('offer keywords to a bare word and not inside quotes', () => {
    expect(manchesterCompletions('so', NAMES).map((x) => x.label)).toEqual(['some', 'some thing']);
    expect(manchesterCompletions("'so", NAMES).map((x) => x.label)).toEqual(['some thing']);
  });

  it('offer every name when nothing is typed', () => {
    expect(manchesterCompletions('', NAMES)).toHaveLength(NAMES.length);
  });
});
