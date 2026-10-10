import { describe, expect, it } from 'vitest';
import type { RecallCitation, RecallFact } from './ask-api';
import {
  ACTIVITY_QUERY,
  AGENTS_QUERY,
  conflictsOf,
  fateLine,
  filterChoices,
  filterFacts,
  graphPrefixes,
  groupByPredicate,
  historyOf,
  isReviewBranch,
  principalName,
  SOURCES_QUERY,
} from './memory';

const facts: RecallFact[] = [
  { s: 'ex:ana', p: 'org:memberOf', o: 'res:payments', citation: 1, status: 'unreviewed' },
  { s: 'ex:ana', p: 'schema:email', o: '"ana@example.org"', citation: 2, status: 'reviewed' },
  { s: 'ex:ana', p: 'ex:worksOn', o: 'res:proj-17', citation: 1 },
  { s: 'ex:ana', p: 'ex:worksOn', o: 'res:proj-17', citation: 3 },
  { s: 'ex:ana', p: 'org:memberOf', o: 'res:payments', citation: 2, status: 'reviewed' },
];
const citations: RecallCitation[] = [
  { id: 1, graph: 'https://example.org/notes/2026-10-08', by: 'urn:x-sparkles:principal:agent-7' },
  { id: 2, graph: 'https://example.org/hr' },
  { id: 3, graph: 'https://example.org/notes/2026-10-01', by: 'urn:x-sparkles:principal:agent-9' },
];
const none = { graphs: new Set<string>(), agents: new Set<string>() };

describe('groupByPredicate', () => {
  it('groups values by predicate and merges their citations', () => {
    expect(groupByPredicate(facts)).toEqual([
      {
        p: 'org:memberOf',
        values: [{ o: 'res:payments', citations: [1, 2], status: 'reviewed' }],
      },
      {
        p: 'schema:email',
        values: [{ o: '"ana@example.org"', citations: [2], status: 'reviewed' }],
      },
      { p: 'ex:worksOn', values: [{ o: 'res:proj-17', citations: [1, 3], status: undefined }] },
    ]);
  });
});

describe('filters', () => {
  it('offers the graphs and agents of the citations', () => {
    expect(filterChoices({ citations })).toEqual({
      graphs: [
        'https://example.org/hr',
        'https://example.org/notes/2026-10-01',
        'https://example.org/notes/2026-10-08',
      ],
      agents: ['urn:x-sparkles:principal:agent-7', 'urn:x-sparkles:principal:agent-9'],
    });
  });

  it('keeps the facts whose citation passes', () => {
    expect(filterFacts(facts, citations, none)).toBe(facts);
    const hr = filterFacts(facts, citations, {
      graphs: new Set(['https://example.org/hr']),
      agents: new Set(),
    });
    expect(hr.map((f) => f.citation)).toEqual([2, 2]);
    const agent = filterFacts(facts, citations, {
      graphs: new Set(),
      agents: new Set(['urn:x-sparkles:principal:agent-9']),
    });
    expect(agent.map((f) => f.citation)).toEqual([3]);
  });

  it('lists the history of a subject, newest invalidation first', () => {
    const h = [
      {
        s: 'ex:ana',
        p: 'org:memberOf',
        o: 'ex:platform',
        graph: 'g1',
        reifier: 'r1',
        invalidatedAt: '2026-10-08T09:14:03Z',
      },
      { s: 'ex:kai', p: 'p', o: 'o', graph: 'g1', reifier: 'r2' },
      {
        s: 'ex:ana',
        p: 'ex:due',
        o: '"x"',
        graph: 'g2',
        reifier: 'r3',
        invalidatedAt: '2026-10-09T00:00:00Z',
      },
    ];
    expect(historyOf(h, 'ex:ana', none).map((x) => x.reifier)).toEqual(['r3', 'r1']);
    expect(
      historyOf(h, 'ex:ana', { graphs: new Set(['g1']), agents: new Set() }).map((x) => x.reifier),
    ).toEqual(['r1']);
    expect(historyOf(undefined, 'ex:ana', none)).toEqual([]);
  });

  it('says what became of a fact', () => {
    const h = {
      s: 'ex:ana',
      p: 'org:memberOf',
      o: 'ex:platform',
      graph: 'g1',
      reifier: '<urn:uuid:r1>',
      invalidatedAt: '2026-10-08T09:14:03Z',
    };
    expect(fateLine({ ...h, replacedBy: '<urn:uuid:r2>' }, 3)).toBe(
      'superseded on 2026-10-08 by [3]',
    );
    expect(fateLine({ ...h, replacedBy: '<urn:uuid:r2>' })).toBe(
      'superseded on 2026-10-08 by <urn:uuid:r2>',
    );
    expect(fateLine(h)).toBe('retracted on 2026-10-08');
    expect(fateLine({ ...h, invalidatedAt: undefined })).toBe('retracted');
  });

  it('finds the conflicts of a subject', () => {
    const c = [
      { s: 'ex:ana', p: 'ex:startDate', values: [] },
      { s: 'ex:kai', p: 'ex:startDate', values: [] },
    ];
    expect(conflictsOf(c, 'ex:ana')).toHaveLength(1);
  });
});

describe('names', () => {
  it('shortens principals', () => {
    expect(principalName('urn:x-sparkles:principal:agent-7')).toBe('agent-7');
    expect(principalName('<urn:x-sparkles:principal:agent-7>')).toBe('agent-7');
    expect(principalName('ex:someone')).toBe('ex:someone');
  });

  it('reduces graphs to their prefixes', () => {
    expect(
      graphPrefixes([
        'https://example.org/memory/agents/agent-7/s1',
        'https://example.org/memory/agents/agent-7/s2',
        'https://example.org/notes/2026-10-08',
      ]),
    ).toEqual(['https://example.org/memory/agents/agent-7/', 'https://example.org/notes/']);
  });

  it('knows review branches', () => {
    expect(isReviewBranch('proposals/agent-7/inbox')).toBe(true);
    expect(isReviewBranch('proposals.agent-7.inbox')).toBe(true);
    expect(isReviewBranch('ingest.standup-1')).toBe(true);
    expect(isReviewBranch('proposalsx')).toBe(false);
    expect(isReviewBranch('ingest/standup-1')).toBe(true);
    expect(isReviewBranch('review/x')).toBe(true);
    expect(isReviewBranch('scratch-s2')).toBe(true);
    expect(isReviewBranch('main')).toBe(false);
    expect(isReviewBranch('dev')).toBe(false);
  });
});

describe('the Memory page queries', () => {
  it('are bounded', () => {
    for (const q of [AGENTS_QUERY, SOURCES_QUERY, ACTIVITY_QUERY])
      expect(q).toMatch(/\bLIMIT \d+$/);
    expect(ACTIVITY_QUERY).toMatch(/LIMIT 20$/);
  });
});
