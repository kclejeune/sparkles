// The pure parts of the Ask bar (C18 Phase 2): the step indicator, row citations in the
// summary, the failure words, graph variables, follow-up context, the remembered run
// mode, the routing record and the event stream parser.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SseParser, type AskEvent, type AskUsage } from './ask-api';
import {
  answeredBy,
  askedFromRecords,
  askFailure,
  askPrefsKey,
  followUpContext,
  graphColumns,
  loadAskPrefs,
  MAX_TURNS,
  nextStep,
  routingLines,
  saveAskPrefs,
  summaryLabel,
  summaryParts,
} from './ask';

describe('the step indicator', () => {
  const o = { run: true, summary: true };
  const ev = (e: AskEvent) => nextStep(e, o);
  it('names the step that comes next', () => {
    expect(ev({ event: 'ground', data: {} })).toBe('Writing a query');
    expect(
      ev({
        event: 'draft',
        data: {
          attempt: 1,
          role: 'draft',
          provider: 'p',
          model: 'm',
          query: 'ASK {}',
        },
      }),
    ).toBe('Checking');
    expect(
      ev({
        event: 'check',
        data: { dataset: 'org', ok: true, issues: [], prefixes: {} },
      }),
    ).toBe('Running');
    expect(
      nextStep(
        {
          event: 'check',
          data: { dataset: 'org', ok: true, issues: [], prefixes: {} },
        },
        { run: false, summary: true },
      ),
    ).toBe('Finishing');
    expect(
      ev({
        event: 'check',
        data: { dataset: 'org', ok: false, issues: [], prefixes: {} },
      }),
    ).toBe('Repairing');
    expect(ev({ event: 'diagnosis', data: { attempt: 2 } })).toBe('Repairing (2 of 2)');
    expect(
      ev({
        event: 'escalate',
        data: {
          role: 'repair',
          from: { provider: 'a', model: 'small' },
          to: { provider: 'b', model: 'large' },
          signal: 'check-failed',
        },
      }),
    ).toBe('Asking large');
    expect(ev({ event: 'run', data: { attempt: 1, rows: 0 } })).toBe(
      'Looking for what matched nothing',
    );
    expect(
      ev({
        event: 'result',
        data: { query: 'ASK {}', results: { queryType: 'ASK' } as never },
      }),
    ).toBe('Summarizing');
    expect(ev({ event: 'result', data: { query: 'ASK {}' } })).toBe('Finishing');
    expect(ev({ event: 'summary', data: { text: '', citations: [] } })).toBeNull();
  });
});

describe('row citations', () => {
  it('cuts the text at its markers and keeps only cited rows', () => {
    const parts = summaryParts(
      'Four people work there [1–4]. Kai joined last [1]. Bo left [9]. See [2, 3].',
      [1, 2, 3, 4],
    );
    expect(parts).toEqual([
      { text: 'Four people work there ' },
      { text: '[1–4]', rows: [1, 2, 3, 4] },
      { text: '. Kai joined last ' },
      { text: '[1]', rows: [1] },
      { text: '. Bo left [9]. See ' },
      { text: '[2, 3]', rows: [2, 3] },
      { text: '.' },
    ]);
    expect(summaryParts('No markers.', [1])).toEqual([{ text: 'No markers.' }]);
    expect(summaryParts('[1-2]', [2])).toEqual([{ text: '[1-2]', rows: [2] }]);
  });

  it('labels the summary as generated with the rows it cites', () => {
    expect(summaryLabel({ text: '', citations: [1, 1, 3] }, 12)).toBe(
      'generated · cites 2 of 12 rows',
    );
    expect(summaryLabel({ text: '', citations: [] }, 3)).toBe('generated · cites no row');
  });
});

describe('failure states', () => {
  it('says what went wrong in words', () => {
    expect(askFailure('provider-unavailable')).toBe('The model provider is not responding.');
    expect(askFailure('timeout')).toBe('The model provider is not responding.');
    expect(askFailure('budget-exceeded')).toBe(
      "This dataset's question budget for today is used up.",
    );
    expect(askFailure('budget-exceeded', undefined, '2026-10-10T00:00:00Z')).toMatch(
      /^This dataset's question budget for today is used up\. It resets at /,
    );
    expect(askFailure('no-valid-query')).toBe(
      'Sparkles could not write a valid query for this question.',
    );
    expect(askFailure('unanswerable')).toBe('The data does not seem to describe this.');
    expect(askFailure('other', 'Something odd')).toBe('Something odd');
  });
});

describe('graph variables', () => {
  it('preselect the pickers when the columns exist', () => {
    expect(
      graphColumns({ subject: '?team', predicate: '?rel', object: '?other' }, [
        'team',
        'rel',
        'other',
      ]),
    ).toEqual({ s: 'team', p: 'rel', o: 'other' });
    expect(graphColumns({ subject: 'a', object: 'b' }, ['a', 'b'])).toEqual({
      s: 'a',
      p: '',
      o: 'b',
    });
    expect(graphColumns({ subject: '?x', object: '?y' }, ['a', 'b'])).toBeNull();
    expect(graphColumns(null, ['a'])).toBeNull();
  });
});

describe('follow-ups', () => {
  it('carry the earlier turns of an answered tab', () => {
    expect(followUpContext(undefined, 'ASK {}')).toEqual([]);
    expect(followUpContext({ question: 'Q', original: 'q' }, 'q')).toEqual([]);
    const q = {
      question: 'Who is on payments?',
      original: 'q1',
      askId: 'a1',
      context: [{ question: 'Earlier', query: 'q0' }],
    };
    expect(followUpContext(q, 'q1 edited')).toEqual([
      { question: 'Earlier', query: 'q0' },
      { question: 'Who is on payments?', query: 'q1 edited' },
    ]);
    const long = {
      ...q,
      context: Array.from({ length: 7 }, (_, i) => ({
        question: `Q${i}`,
        query: `q${i}`,
      })),
    };
    const ctx = followUpContext(long, 'q');
    expect(ctx).toHaveLength(MAX_TURNS);
    expect(ctx.at(-1)?.question).toBe('Who is on payments?');
  });
});

describe('the run mode', () => {
  afterEach(() => vi.unstubAllGlobals());
  it('is remembered per principal and starts as preview', () => {
    const store = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => store.set(k, v),
      removeItem: (k: string) => store.delete(k),
    });
    expect(askPrefsKey(null)).toBe('sparkles.askPrefs.local');
    expect(loadAskPrefs('ana')).toEqual({
      mode: 'preview',
      summaryCollapsed: false,
    });
    saveAskPrefs('ana', { mode: 'run', summaryCollapsed: true });
    expect(loadAskPrefs('ana')).toEqual({
      mode: 'run',
      summaryCollapsed: true,
    });
    expect(loadAskPrefs('kai').mode).toBe('preview');
    store.set('sparkles.askPrefs.bo', '{"mode":"other"}');
    expect(loadAskPrefs('bo').mode).toBe('preview');
  });
});

describe('the routing record', () => {
  const u: AskUsage = {
    answeredBy: { role: 'repair', provider: 'cloud', model: 'large' },
    steps: [
      {
        role: 'draft',
        provider: 'local',
        model: 'small',
        outcome: 'ok',
        latencyMs: 120,
      },
      {
        role: 'repair',
        provider: 'cloud',
        model: 'large',
        outcome: 'ok',
        latencyMs: 900,
        signal: 'check-failed',
      },
    ],
    escalations: [
      {
        role: 'repair',
        from: { provider: 'local', model: 'small' },
        to: { provider: 'cloud', model: 'large' },
        signal: 'check-failed',
      },
    ],
  };
  it('names the model that answered and every step', () => {
    expect(answeredBy(u)).toBe('cloud · large');
    expect(answeredBy(undefined)).toBeNull();
    expect(routingLines(u)).toEqual([
      'draft: local · small (ok, 120 ms)',
      'repair: cloud · large (ok, 900 ms, check-failed)',
      'repair moved from small to large (check-failed)',
    ]);
  });

  it('turns the server history into Asked entries', () => {
    expect(
      askedFromRecords([
        {
          id: '1',
          at: 't',
          question: 'Q',
          query: 'q',
          result: 'answered',
          outcome: 'none',
        },
        {
          id: '2',
          at: 't',
          question: 'No query',
          result: 'failed',
          outcome: 'none',
        },
      ]),
    ).toEqual([{ id: '1', question: 'Q', query: 'q', at: 't' }]);
  });
});

describe('the event stream parser', () => {
  it('splits events across chunks and skips comments', () => {
    const p = new SseParser();
    expect(p.push('event: draft\ndata: {"a":')).toEqual([]);
    expect(p.push('1}\n\n: keep-alive\n\nevent: usage\r\ndata: {}\r\n\r\n')).toEqual([
      { event: 'draft', data: '{"a":1}' },
      { event: 'usage', data: '{}' },
    ]);
    expect(p.push('data: x\ndata: y\n\n')).toEqual([{ event: 'message', data: 'x\ny' }]);
  });
});
