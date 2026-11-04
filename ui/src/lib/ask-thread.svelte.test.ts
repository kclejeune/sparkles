// The live Ask thread's state (ask-thread.svelte.ts): a sign-in as someone else never
// shows the last person's thread, Show rows reruns on the turn's branch and falls back to
// the head only for a state the server no longer keeps, and a Query page state that does
// not parse asks nothing.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from './api';
import * as askApi from './ask-api';
import { askThread } from './ask-thread.svelte';
import type { ThreadTurn } from './ask-thread';

class MemoryStorage {
  #m = new Map<string, string>();
  getItem(k: string) {
    return this.#m.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.#m.set(k, v);
  }
  removeItem(k: string) {
    this.#m.delete(k);
  }
  clear() {
    this.#m.clear();
  }
  entries() {
    return [...this.#m.entries()];
  }
}

const turn = (over: Partial<ThreadTurn> = {}): ThreadTurn => ({
  key: 'k',
  question: 'Who is on payments?',
  status: 'stored',
  trail: [],
  result: { query: 'SELECT * { ?s ?p ?o }', commit: 4 },
  ...over,
});

beforeEach(() => {
  vi.stubGlobal('sessionStorage', new MemoryStorage());
  vi.stubGlobal('localStorage', new MemoryStorage());
  vi.spyOn(askApi, 'assistantSettings').mockResolvedValue(null as never);
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('the live thread', () => {
  it('keeps each person their own thread on the same dataset', async () => {
    await askThread.setDataset('org', 'alice');
    askThread.newThread();
    askThread.thread = { id: 't1', turns: [turn()] };
    askThread.draft = 'half a question';
    // Bob signs in on the same dataset: nothing of Alice's stays on screen
    await askThread.setDataset('org', 'bob');
    expect(askThread.thread.turns).toEqual([]);
    expect(askThread.draft).toBe('');
    // and the tab's storage keeps a thread per person
    const keys = Object.keys(
      Object.fromEntries((sessionStorage as unknown as MemoryStorage).entries()),
    );
    expect(keys).toEqual(['sparkles.askThread:["alice","org"]']);
  });

  it('keeps names with dots apart', async () => {
    await askThread.setDataset('org', 'alice.team');
    askThread.newThread();
    askThread.thread = { id: 't1', turns: [turn()] };
    askThread.newThread();
    await askThread.setDataset('team.org', 'alice');
    const keys = (sessionStorage as unknown as MemoryStorage).entries().map(([k]) => k);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).toContain('sparkles.askThread:["alice.team","org"]');
  });

  it('drops a response that comes back after another person signs in', async () => {
    await askThread.setDataset('org', 'alice');
    let release: (v: askApi.AskRecord[]) => void = () => {};
    vi.spyOn(askApi, 'askHistory').mockReturnValueOnce(
      new Promise<askApi.AskRecord[]>((r) => (release = r)),
    );
    const pending = askThread.reopen('t1');
    await askThread.setDataset('org', 'bob');
    release([
      {
        id: 't1',
        thread: 't1',
        principal: 'alice',
        at: '2026-11-03T10:00:00Z',
        question: "Alice's question",
        query: 'SELECT * {}',
        result: 'answered',
        outcome: 'none',
      } as askApi.AskRecord,
    ]);
    await pending;
    expect(askThread.thread.turns).toEqual([]);
  });

  it("asks on the page's branch and state without a chosen resource", async () => {
    await askThread.setDataset('org', 'alice');
    askThread.newThread();
    askThread.pageContext = null;
    askThread.pageTarget = { branch: 'dev', at: 'commit:3' };
    const stream = vi.spyOn(askApi, 'askStream').mockResolvedValue(undefined as never);
    await askThread.ask('What is here?');
    expect(stream.mock.calls[0][1]).toMatchObject({ branch: 'dev', at: 'commit:3' });
    expect(askThread.thread.turns[0]).toMatchObject({ branch: 'dev', readAt: 'commit:3' });
    askThread.pageTarget = null;
  });

  it('reruns on the branch the turn read, and falls back only when the state is gone', async () => {
    await askThread.setDataset('org', 'alice');
    const q = vi.spyOn(api, 'query');
    q.mockResolvedValueOnce({ results: { vars: [], rows: [] } } as never);
    const t = turn({ branch: 'dev' });
    expect(await askThread.rerun(t)).toBeNull();
    expect(q.mock.calls[0][0]).toBe('org@dev');
    expect(q.mock.calls[0][2]).toMatchObject({ at: 'commit:4' });

    q.mockRejectedValueOnce(new api.ApiError(410, 'gone', { code: 'history-gone' }));
    q.mockResolvedValueOnce({ results: { vars: [], rows: [] } } as never);
    expect(await askThread.rerun(turn())).toMatch(/no longer kept/);
    expect(q.mock.calls[2][2]).not.toHaveProperty('at');

    q.mockRejectedValueOnce(new api.ApiError(504, 'timeout', { code: 'timeout' }));
    await expect(askThread.rerun(turn())).rejects.toThrow('timeout');
    expect(q).toHaveBeenCalledTimes(4);
  });

  it('asks nothing when the Query page state does not parse', async () => {
    await askThread.setDataset('org', 'alice');
    askThread.newThread();
    const stream = vi.spyOn(askApi, 'askStream');
    const detach = askThread.attach({ extras: () => null, begin: () => null, open: () => {} });
    await askThread.ask('Who is on payments?');
    detach();
    expect(stream).not.toHaveBeenCalled();
    expect(askThread.thread.turns).toEqual([]);
  });
});
