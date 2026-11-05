import { afterEach, describe, expect, it, vi } from 'vitest';
import { ClassificationFrame } from './classification-frame.svelte';
import type {
  ClassificationResult,
  ClassificationView,
  KeptClassification,
} from './classification-read';

const view: ClassificationView = {
  target: 'ds@dev',
  selection: { graph: 'urn:open', imports: true },
  iri: 'urn:A',
  commit: 7,
  mode: 'lenient',
  languages: ['en'],
};
const result: ClassificationResult = {
  resultId: 'result-a',
  classifiedCommit: 7,
  boundStoreId: 'branch-a',
  familyId: 'family',
  inputFingerprint: 'input',
  sourceFingerprint: 'source',
  contentFingerprint: 'content',
  imports: 'dataset',
  mode: 'lenient',
  assertions: false,
  elOnly: false,
  partialRequested: false,
  expansion: 'cautious',
  sourceCounts: { requestedGraphs: 1, resolvedGraphs: 1, imports: 0, warnings: 0 },
  capabilities: ['namedClassDirectParents'],
  justifications: 'unavailable',
};
const name = (iri: string) => ({ iri, label: iri + ' label', labelLang: 'en' });
function available(overrides: Partial<KeptClassification> = {}): KeptClassification {
  return {
    commit: 7,
    status: 'available',
    reason: null,
    result,
    outcome: {
      tier: 'el',
      partial: false,
      complete: true,
      consistency: 'consistent',
      excludedCount: 0,
      incompleteCount: 0,
    },
    entity: { iri: 'urn:A', present: true, unsatisfiable: false },
    activity: null,
    staleCommit: null,
    page: null,
    ...overrides,
  };
}
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}
type Call = { url: URL; init: RequestInit };
function server(answer: (call: Call) => Response | Promise<Response>) {
  const calls: Call[] = [];
  vi.stubGlobal('fetch', (url: string, init: RequestInit = {}) => {
    const call = { url: new URL(url, 'http://localhost'), init };
    calls.push(call);
    return answer(call);
  });
  return calls;
}
function page(call: Call, overrides: Partial<KeptClassification> = {}) {
  const q = call.url.searchParams;
  const part = q.get('part');
  return response(
    available({
      page:
        part === 'status'
          ? null
          : {
              part: part as 'parents',
              about: part === 'parents' || part === 'equivalents' ? q.get('about') : null,
              offset: Number(q.get('offset') ?? 0),
              limit: 100,
              total: 1,
              items: [name(part === 'parents' ? 'urn:Parent' : 'urn:Alias')],
              nextOffset: null,
            },
      ...overrides,
    }),
  );
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
afterEach(() => vi.unstubAllGlobals());

describe('one pinned class frame', () => {
  it('does not retry an identical parent view when its containing term refreshes', async () => {
    const calls = server(page);
    const controller = new ClassificationFrame();
    await controller.reset(view);
    await controller.reset({
      ...view,
      selection: { ...view.selection },
      languages: [...view.languages],
    });
    expect(calls).toHaveLength(3);
    expect(controller.parts.parents.items).toHaveLength(1);
  });
  it('changes label languages under the same immutable pin, without computing', async () => {
    const calls = server(page);
    const controller = new ClassificationFrame();
    await controller.reset(view);
    await controller.reset({ ...view, languages: ['fr'] });
    expect(calls).toHaveLength(6);
    for (const call of calls.slice(3)) {
      expect(call.url.searchParams.get('resultId')).toBe('result-a');
      expect(call.url.searchParams.get('lang')).toBe('fr');
      expect(call.init.method ?? 'GET').toBe('GET');
    }
  });
  it('autoloads through GET only, with exact branch, graph, commit, language and one pin', async () => {
    const calls = server(page);
    const controller = new ClassificationFrame();
    await controller.reset(view);
    expect(controller.parts.parents.items).toEqual([name('urn:Parent')]);
    expect(controller.parts.equivalents.items).toEqual([name('urn:Alias')]);
    expect(calls).toHaveLength(3);
    for (const { url, init } of calls) {
      expect(init.method ?? 'GET').toBe('GET');
      expect(init.cache).toBe('no-store');
      expect(url.pathname).toContain('ds/classification');
      expect(url.searchParams.get('branch')).toBe('dev');
      expect(url.searchParams.get('graph')).toBe('urn:open');
      expect(url.searchParams.get('at')).toBe('commit:7');
      expect(url.searchParams.get('lang')).toBe('en');
      expect(url.searchParams.has('inferences')).toBe(false);
      if (url.searchParams.get('part') !== 'status')
        expect(url.searchParams.get('resultId')).toBe('result-a');
    }
  });
  it.each(['unavailable', 'running', 'mismatch'] as const)(
    'represents %s without computation or borrowed outcome',
    async (status) => {
      const calls = server(() =>
        response(available({ status, result: null, outcome: null, entity: null })),
      );
      const controller = new ClassificationFrame();
      await controller.reset(view);
      expect(controller.reply?.status).toBe(status);
      expect(controller.reply?.outcome).toBeNull();
      expect(calls).toHaveLength(1);
      await controller.refresh();
      expect(calls.every((c) => c.init.method !== 'POST')).toBe(true);
    },
  );
  it('keeps successful rows and exact provenance when another page fails, and retries GET', async () => {
    let fail = true;
    const calls = server((c) =>
      c.url.searchParams.get('part') === 'equivalents' && fail
        ? response({ error: 'read failed' }, 503)
        : page(c),
    );
    const controller = new ClassificationFrame();
    await controller.reset(view);
    expect(controller.parts.parents.items).toHaveLength(1);
    expect(controller.parts.equivalents.error).toContain('read failed');
    expect(controller.reply?.result).toEqual(result);
    fail = false;
    await controller.more('equivalents');
    expect(controller.parts.equivalents.error).toBeNull();
    expect(controller.parts.equivalents.items).toHaveLength(1);
    expect(calls.every((c) => c.init.method !== 'POST')).toBe(true);
  });
  it('invalidates all assembled clauses on a replaced result and ignores a late successful page', async () => {
    const late = deferred<Response>();
    const calls = server((c) =>
      c.url.searchParams.get('part') === 'parents'
        ? late.promise
        : c.url.searchParams.get('part') === 'equivalents'
          ? response(available({ status: 'mismatch', result: null, outcome: null }))
          : page(c),
    );
    const controller = new ClassificationFrame();
    const loading = controller.reset(view);
    await vi.waitFor(() => expect(calls).toHaveLength(3));
    await vi.waitFor(() => expect(controller.reply?.status).toBe('mismatch'));
    late.resolve(page(calls[1]));
    await loading;
    expect(controller.parts.parents.items).toEqual([]);
    expect(controller.parts.equivalents.items).toEqual([]);
    expect(controller.reply?.result).toBeNull();
  });
  it('discards late status after entity/scope/commit/language replacement', async () => {
    const old = deferred<Response>();
    let initial = true;
    server((c) => {
      if (initial) {
        initial = false;
        return old.promise;
      }
      return response(available({ commit: 8, status: 'unavailable', result: null, outcome: null }));
    });
    const controller = new ClassificationFrame();
    const before = controller.reset(view);
    await controller.reset({
      ...view,
      iri: 'urn:Other',
      commit: 8,
      languages: ['fr'],
      selection: { graph: 'urn:other', imports: false },
    });
    old.resolve(response(available()));
    await before;
    expect(controller.reply?.commit).toBe(8);
    expect(controller.reply?.status).toBe('unavailable');
    expect(controller.parts.parents.items).toEqual([]);
  });
  it('makes one deliberate bounded POST and offers no reclassification continuation for isolated output', async () => {
    const items = Array.from({ length: 100 }, (_, i) => name(`urn:E${i}`));
    const calls = server((c) =>
      c.init.method === 'POST'
        ? response({
            commit: 7,
            frame: {
              iri: 'urn:A',
              result,
              outcome: available().outcome,
              unsatisfiable: false,
              parents: { items: [], total: 0, truncated: false, limit: 100 },
              equivalents: { items, total: 101, truncated: true, limit: 100 },
              sources: {
                items: [
                  { kind: 'requestedGraph', ordinal: 0, graph: 'urn:open' },
                  { kind: 'resolvedGraph', ordinal: 1, graph: 'urn:open', requested: true },
                ],
                total: 2,
                truncated: false,
              },
              excluded: { items: [], total: 0, truncated: false, limit: 100 },
              retention: { kept: false, resultId: null },
            },
          })
        : response(available({ status: 'unavailable', result: null, outcome: null })),
    );
    const controller = new ClassificationFrame();
    await controller.reset(view);
    await controller.classify();
    expect(controller.isolated).toBe(true);
    expect(controller.parts.equivalents.items).toHaveLength(100);
    expect(controller.parts.equivalents.total).toBe(101);
    await controller.more('equivalents');
    expect(calls).toHaveLength(2);
    const post = calls[1];
    expect(post.url.searchParams.get('at')).toBe('commit:7');
    expect(JSON.parse(String(post.init.body))).toMatchObject({
      projection: 'frame',
      about: 'urn:A',
      graph: 'urn:open',
      limit: 100,
      lang: ['en'],
    });
  });
});
