import { describe, expect, it } from 'vitest';
import { askPayload, decodeHandoff, encodeHandoff, toBase64Url } from './handoff';

describe('askPayload', () => {
  it('reads the ask member of a fragment', () => {
    expect(askPayload('#ask=abc')).toBe('abc');
    expect(askPayload('ask=abc')).toBe('abc');
    expect(askPayload('#x=1&ask=abc')).toBe('abc');
    expect(askPayload('#other')).toBeNull();
    expect(askPayload('')).toBeNull();
  });
});

describe('decodeHandoff', () => {
  const link = {
    dataset: 'org',
    query: 'SELECT ?p WHERE { ?p ex:memberOf res:payments }',
    question: 'Who is on the payments team?',
    explanation: 'Finds the members of the payments team.',
    assumptions: ['"payments team" = res:payments'],
  };

  it('round-trips a payload', () => {
    expect(decodeHandoff(encodeHandoff(link))).toEqual(link);
  });

  it('decodes base64url without padding and UTF-8 text', () => {
    const payload = toBase64Url(
      JSON.stringify({ dataset: 'org', query: 'ASK {}', question: 'Où ?' }),
    );
    expect(payload).not.toMatch(/[=+/]/);
    expect(decodeHandoff(payload).question).toBe('Où ?');
  });

  it('keeps the branch and the commit', () => {
    const h = decodeHandoff(encodeHandoff({ ...link, branch: 'dev', atCommit: 41 }));
    expect(h.branch).toBe('dev');
    expect(h.atCommit).toBe(41);
  });

  it('refuses damaged or incomplete payloads', () => {
    expect(() => decodeHandoff('!!!')).toThrow(/base64url|damaged/);
    expect(() => decodeHandoff(toBase64Url('not json'))).toThrow(/damaged/);
    expect(() => decodeHandoff(toBase64Url('[1]'))).toThrow(/damaged/);
    expect(() => decodeHandoff(toBase64Url(JSON.stringify({ query: 'ASK {}' })))).toThrow(
      /no dataset/,
    );
    expect(() =>
      decodeHandoff(toBase64Url(JSON.stringify({ dataset: 'org', query: ' ' }))),
    ).toThrow(/no query/);
  });

  it('enforces the limits of share_query', () => {
    const big = (n: number) => 'x'.repeat(n);
    expect(() => decodeHandoff(encodeHandoff({ ...link, question: big(2001) }))).toThrow(
      /question/,
    );
    expect(() => decodeHandoff(encodeHandoff({ ...link, explanation: big(401) }))).toThrow(
      /explanation/,
    );
    expect(() =>
      decodeHandoff(encodeHandoff({ ...link, assumptions: ['a', 'b', 'c', 'd', 'e', 'f'] })),
    ).toThrow(/assumptions/);
    expect(() => decodeHandoff(toBase64Url(JSON.stringify({ ...link, assumptions: [1] })))).toThrow(
      /assumptions/,
    );
    expect(() => decodeHandoff(toBase64Url(JSON.stringify({ ...link, atCommit: -1 })))).toThrow(
      /commit/,
    );
  });

  it('drops empty optional members', () => {
    const h = decodeHandoff(
      toBase64Url(
        JSON.stringify({ dataset: 'org', query: 'ASK {}', question: ' ', assumptions: [] }),
      ),
    );
    expect(h).toEqual({ dataset: 'org', query: 'ASK {}' });
  });
});
