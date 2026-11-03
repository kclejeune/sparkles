// The pure part of the Ask thread (C18 §6.2): folding events into a turn and its trail,
// the context a page lends, follow-up context, the summaries of the folds, records as
// stored turns and threads, and the targets of the actions.
import { describe, expect, it } from 'vitest';
import type { AskEvent, AskRecord } from './ask-api';
import {
  chipAbout,
  chipSuggestions,
  chipText,
  chipTypeOf,
  citedIris,
  dayGroup,
  exploreTarget,
  foldLines,
  reduceTurn,
  threadContext,
  threadLine,
  threadsFromRecords,
  turnFromRecord,
  type ThreadTurn,
} from './ask-thread';

const running = (): ThreadTurn => ({
  key: 'k',
  question: 'Who is on payments?',
  status: 'running',
  trail: [],
});

const Q = 'SELECT ?p WHERE { ?p ex:memberOf res:payments }';

describe('a running turn', () => {
  it('lands each step on its trail and keeps the parts', () => {
    const t = running();
    const o = { run: true, summary: true };
    const feed = (e: AskEvent) => reduceTurn(t, e, o);
    feed({
      event: 'ground',
      data: {
        entities: [
          {
            text: 'payments',
            verdict: 'exact',
            candidates: [{ iri: 'urn:pay', label: 'Payments' }],
          },
        ],
        communities: ['urn:c1'],
      },
    });
    expect(t.trail.map((s) => s.name)).toEqual(['Entities']);
    expect(t.trail[0].detail).toBe('Payments, linked');
    expect(t.entities).toEqual(['urn:pay']);
    expect(t.communities).toEqual(['urn:c1']);
    feed({
      event: 'draft',
      data: { attempt: 1, role: 'draft', provider: 'mock', model: 'small', query: Q },
    });
    expect(t.step).toBe('Checking');
    feed({ event: 'check', data: { dataset: 'org', ok: true, issues: [], prefixes: {} } });
    expect(t.trail.find((s) => s.key === 'query')?.detail).toBe('checked, no issues');
    feed({ event: 'run', data: { attempt: 1, rows: 2 } });
    expect(t.trail.find((s) => s.key === 'rows')?.detail).toBe('2 rows');
    feed({ event: 'summary', data: { text: 'Two [1] [2].', citations: [1, 2] } });
    expect(t.step).toBe('Finishing');
    feed({ event: 'usage', data: { askId: 'a1', threadId: 'a1', outcome: 'answered', steps: [] } });
    expect(t.id).toBe('a1');
    expect(t.mode).toBe('local');
    feed({ event: 'error', data: { code: 'timeout', message: 'late' } });
    expect(t.failure).toBe('The model provider is not responding.');
  });

  it('marks a failed check', () => {
    const t = running();
    reduceTurn(
      t,
      {
        event: 'check',
        data: {
          dataset: 'org',
          ok: false,
          issues: [{ code: 'x', severity: 'error', message: 'bad' }],
          prefixes: {},
        },
      },
      { run: true, summary: false },
    );
    expect(t.trail[0]).toMatchObject({ name: 'Query', detail: '1 error', ok: false });
  });
});

describe('the context of a page', () => {
  const entity = {
    kind: 'entity' as const,
    iri: 'urn:x',
    label: 'mitochondrion',
    type: 'class' as const,
  };
  it('words the chip and suggests questions that fit it', () => {
    expect(chipText(entity)).toBe('About mitochondrion');
    expect(chipText({ kind: 'commit', label: 'c', commit: 18 })).toBe('As of commit 18');
    expect(chipSuggestions(entity)[0]).toBe('What are its subclasses?');
    expect(chipSuggestions({ ...entity, type: 'concept' })[0]).toBe('Which concepts are narrower?');
    expect(chipSuggestions(null)).toEqual([]);
  });
  it('names a resource only for an entity', () => {
    expect(chipAbout(entity)).toEqual({ iri: 'urn:x', label: 'mitochondrion', kind: 'class' });
    expect(chipAbout({ kind: 'query', label: 'q', query: Q })).toBeUndefined();
  });
  it('reads the kind from an entity page', () => {
    expect(chipTypeOf(['class'])).toBe('class');
    expect(chipTypeOf(['objectProperty'])).toBe('property');
    expect(chipTypeOf(['concept'])).toBe('concept');
    expect(chipTypeOf([])).toBe('resource');
  });
});

describe('follow-up context', () => {
  const done = (question: string, query?: string): ThreadTurn => ({
    key: question,
    question,
    status: 'done',
    trail: [],
    result: query ? { query } : undefined,
  });
  it('carries the answered turns, the last five, after a query chip', () => {
    const turns = Array.from({ length: 7 }, (_, i) => done(`q${i}`, `SELECT ${i}`));
    turns.push(done('no query'));
    const ctx = threadContext(turns, { kind: 'query', label: 'tab', query: 'SELECT tab' });
    expect(ctx.map((t) => t.question)).toEqual(['q2', 'q3', 'q4', 'q5', 'q6']);
    expect(threadContext([], { kind: 'query', label: 'tab', question: 'Q?', query: 'S' })).toEqual([
      { question: 'Q?', query: 'S' },
    ]);
  });
  it('does not carry the chip twice when it is a turn opened in a tab', () => {
    const ctx = threadContext([done('first', Q)], {
      kind: 'query',
      label: 't',
      question: 'first',
      query: Q,
    });
    expect(ctx).toEqual([{ question: 'first', query: Q }]);
  });
});

const record = (over: Partial<AskRecord> = {}): AskRecord => ({
  id: 'r1',
  at: '2026-11-03T10:00:00Z',
  question: 'Who is on payments?',
  query: Q,
  commit: 4,
  result: 'answered',
  outcome: 'none',
  ...over,
});

describe('records of the history', () => {
  it('become stored turns with their answer, steps and resource', () => {
    const t = turnFromRecord(
      record({
        thread: 't1',
        about: { iri: 'urn:pay', label: 'Payments' },
        answer: {
          explanation: 'Members.',
          summary: { text: 'Two [1].', citations: [1] },
          notes: ['n'],
          entities: ['urn:pay'],
        },
        routing: {
          steps: [{ role: 'draft', provider: 'p', model: 'm', outcome: 'ok', latencyMs: 900 }],
        },
      }),
    );
    expect(t.status).toBe('stored');
    expect(t.summary?.text).toBe('Two [1].');
    expect(t.chip).toEqual({ kind: 'entity', iri: 'urn:pay', label: 'Payments' });
    expect(t.result?.query).toBe(Q);
    expect(t.usage?.steps?.length).toBe(1);
    expect(foldLines(t)).toMatchObject({
      rows: 'not stored, run again at commit 4',
      steps: 'draft in 0.9 s',
    });
  });
  it('read a record kept before stored answers, and one whose answer was withheld', () => {
    const old = turnFromRecord(record({ question: 'Q? (about <urn:x>)', outcome: 'accepted' }));
    expect(old.question).toBe('Q?');
    expect(old.summary).toBeUndefined();
    expect(old.feedback).toBe('accepted');
    expect(turnFromRecord(record({ answerWithheld: true })).withheld).toBe(true);
  });
  it('group into threads, the latest first, a record without a thread on its own', () => {
    const list = threadsFromRecords([
      record({ id: 'a', thread: 't1', at: '2026-11-03T10:00:00Z' }),
      record({
        id: 'b',
        thread: 't1',
        at: '2026-11-03T11:00:00Z',
        question: 'And?',
        outcome: 'edited',
      }),
      record({ id: 'old', at: '2026-10-01T10:00:00Z', question: 'Old' }),
      record({
        id: 'g',
        thread: 'g',
        at: '2026-11-02T10:00:00Z',
        question: 'Main areas',
        mode: 'global',
      }),
    ]);
    expect(list.map((t) => t.id)).toEqual(['t1', 'g', 'old']);
    expect(list[0]).toMatchObject({ title: 'Who is on payments?', turns: 2, note: 'edited' });
    expect(threadLine(list[0])).toBe('2 questions, edited');
    expect(threadLine(list[1])).toBe('1 question, from reports');
  });
  it('fall into day groups', () => {
    const now = new Date('2026-11-03T12:00:00');
    expect(dayGroup('2026-11-03T08:00:00', now)).toBe('Today');
    expect(dayGroup('2026-11-02T08:00:00', now)).toBe('Yesterday');
    expect(dayGroup('2026-10-02T08:00:00', now)).toBe('Earlier');
    expect(dayGroup('nope', now)).toBe('Earlier');
  });
});

describe('the actions', () => {
  it('open the cited entity, a linked one, or the first IRI', () => {
    const t: ThreadTurn = {
      ...running(),
      status: 'done',
      summary: { text: '[2]', citations: [2] },
      result: {
        results: {
          queryType: 'SELECT',
          vars: ['p'],
          rows: [[{ type: 'uri', value: 'urn:a' }], [{ type: 'uri', value: 'urn:b' }]],
        } as never,
      },
    };
    expect(citedIris(t)).toEqual(['urn:b']);
    expect(exploreTarget(t)).toBe('urn:b');
    expect(exploreTarget({ ...t, summary: undefined, entities: ['urn:e'] })).toBe('urn:e');
    expect(exploreTarget({ ...t, summary: undefined })).toBe('urn:a');
  });
});
