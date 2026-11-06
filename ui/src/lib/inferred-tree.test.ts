import { afterEach, describe, expect, it, vi } from 'vitest';
import { InferredTree } from './inferred-tree.svelte';
import { InferredPins, type InferredView, type MaterializationSource } from './inferred-read';
import { MaterializedFrame } from './materialized-frame.svelte';
import type { ClassificationResult, KeptClassification } from './classification-read';

const view: InferredView = {
  target: 'ds@dev',
  selection: { graph: null, imports: true },
  commit: 7,
  languages: ['en'],
};
const source: MaterializationSource = {
  kind: 'materialization',
  sourceId: `sha256:${'a'.repeat(64)}`,
  profile: 'rdfs',
  producerCommit: 6,
  commit: 7,
  boundStoreId: 'branch',
  familyId: 'family',
  inputGraphs: ['default'],
  inputGraphCount: 1,
  inputGraphsTruncated: false,
  warnings: [],
  warningCount: 0,
  warningsTruncated: false,
  complete: null,
  consistency: 'unknown',
  justifications: 'unavailable',
};
const node = (iri: string, children = 0) => ({
  iri,
  label: iri,
  labelLang: 'en',
  shortForm: null,
  kinds: ['objectProperty'],
  deprecated: false,
  children,
  parents: 0,
});
type Call = { url: URL; init: RequestInit };
function server(answer: (call: Call) => unknown | Promise<unknown>) {
  const calls: Call[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
    const call = { url: new URL(url, 'http://localhost'), init };
    calls.push(call);
    const body = await answer(call);
    if (body instanceof Response) return body;
    return new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json' } });
  });
  return calls;
}
function hierarchy(call: Call, override: Record<string, unknown> = {}) {
  const q = call.url.searchParams;
  return {
    dataset: 'ds',
    commit: 7,
    kind: 'objectProperty',
    node: q.has('iri') ? node(q.get('iri')!) : null,
    children: [node('urn:r', 1)],
    total: 1,
    offset: 0,
    limit: Number(q.get('limit')),
    next: null,
    nextAfter: null,
    hierarchyBasis: 'directEquivalenceGroups',
    inferenceSource: source,
    order: 'iri',
    ...override,
  };
}
function term(override: Record<string, unknown> = {}) {
  return {
    iri: 'urn:p',
    commit: 7,
    inferenceSource: source,
    axioms: [],
    statements: [],
    statementsTruncated: false,
    names: {},
    warnings: [],
    ...override,
  };
}
afterEach(() => vi.unstubAllGlobals());

describe('source-pinned inferred regions', () => {
  it('retires an obsolete property path-level read after its locate already succeeded', async () => {
    let release!: (value: unknown) => void;
    server((call) => {
      const iri = call.url.searchParams.get('iri');
      if (iri === 'urn:P')
        return new Promise((done) => {
          release = done;
        });
      if (iri === 'urn:A' || iri === 'urn:B') {
        const parent = iri === 'urn:A' ? 'urn:P' : 'urn:Q';
        return hierarchy(call, {
          node: node(iri),
          children: [],
          total: 0,
          ancestors: {
            nodes: [node(parent, 1), node('urn:R', 2)],
            edges: [
              [iri, parent],
              [parent, 'urn:R'],
            ],
            truncated: false,
          },
        });
      }
      return hierarchy(call, {
        node: iri ? node(iri, iri === 'urn:R' ? 2 : 1) : null,
        children:
          iri === 'urn:R'
            ? [node('urn:P', 1), node('urn:Q', 1)]
            : [node(iri === 'urn:Q' ? 'urn:B' : 'urn:R', iri ? 0 : 2)],
        total: iri === 'urn:R' ? 2 : 1,
      });
    });
    const model = new InferredTree();
    model.reset(view, 'ObjectProperty');
    const old = model.reveal('urn:A');
    await vi.waitFor(() => expect(release).toBeTypeOf('function'));
    await model.reveal('urn:B');
    expect(model.tree.children('urn:Q')).toContain('urn:B');
    release(new Response('obsolete parent level', { status: 404 }));
    await old;
    expect(model.tree.roots()).toEqual(['urn:R']);
    expect(model.tree.children('urn:Q')).toContain('urn:B');
    expect(model.error).toBeNull();
    model.drop();
  });

  it('retires an obsolete individual bucket read after its locate already succeeded', async () => {
    let release!: (value: unknown) => void;
    server((call) => {
      const q = call.url.searchParams;
      if (q.get('type') === 'urn:P')
        return new Promise((done) => {
          release = done;
        });
      const iri = q.get('iri');
      const parent = iri === 'urn:A' ? 'urn:P' : 'urn:Q';
      const members = q.has('iri') || q.has('type');
      return {
        commit: 7,
        inferenceSource: source,
        typeBasis: 'materializedNamedTypes',
        groups: members
          ? [{ type: node(parent), total: null }]
          : [
              { type: node('urn:P'), total: 1 },
              { type: node('urn:Q'), total: 1 },
            ],
        individuals: members ? [{ ...node(iri ?? 'urn:B'), types: [node(parent)] }] : [],
        total: members ? 1 : 2,
        next: null,
      };
    });
    const model = new InferredTree();
    model.reset(view, 'NamedIndividual');
    const old = model.reveal('urn:A');
    await vi.waitFor(() => expect(release).toBeTypeOf('function'));
    await model.reveal('urn:B');
    expect(model.individuals.children('@type:urn:Q')).toContain('urn:B');
    release(new Response('obsolete type bucket', { status: 404 }));
    await old;
    expect(model.individuals.children('@type:urn:Q')).toContain('urn:B');
    expect(model.individuals.types['urn:B']).toEqual(['urn:Q']);
    expect(model.error).toBeNull();
    model.drop();
  });
  it('an obsolete kept-class locate cannot publish a failure over a newer successful reveal', async () => {
    let release!: (value: unknown) => void;
    const result: ClassificationResult = {
      resultId: 'kept-a',
      classifiedCommit: 7,
      boundStoreId: 'branch',
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
      capabilities: ['namedClassRoots', 'namedClassDirectChildren'],
      justifications: 'unavailable',
    };
    const row = (iri: string, children = 0) => ({
      iri,
      label: iri,
      labelLang: 'en',
      children,
      parents: 1,
      equivalenceCount: 1,
    });
    server((call) => {
      const q = call.url.searchParams;
      const about = q.get('about');
      if (about === 'urn:A')
        return new Promise((done) => {
          release = done;
        });
      const part = q.get('part') as 'roots' | 'children' | 'parents';
      const rows =
        part === 'roots'
          ? [row('urn:R', 1)]
          : part === 'parents' && about === 'urn:B'
            ? [row('urn:R', 1)]
            : part === 'children' && about === 'urn:R'
              ? [row('urn:B')]
              : [];
      return {
        commit: 7,
        status: 'available',
        result,
        reason: null,
        activity: null,
        staleCommit: null,
        outcome: {
          tier: 'el',
          partial: false,
          complete: true,
          consistency: 'consistent',
          excludedCount: 0,
          incompleteCount: 0,
        },
        entity: about
          ? {
              iri: about,
              present: true,
              representative: about,
              unsatisfiable: false,
              children: about === 'urn:R' ? 1 : 0,
              parents: 1,
              equivalenceCount: 1,
            }
          : null,
        page: {
          part,
          about,
          offset: Number(q.get('offset') ?? 0),
          limit: Number(q.get('limit')),
          total: rows.length,
          items: rows,
          nextOffset: null,
        },
      };
    });
    const model = new InferredTree();
    model.reset(view, 'Class');
    const old = model.reveal('urn:A');
    await vi.waitFor(() => expect(release).toBeTypeOf('function'));
    await model.reveal('urn:B');
    expect(model.tree.children('urn:R')).toContain('urn:B');
    release(new Response(JSON.stringify({ message: 'obsolete class is absent' }), { status: 404 }));
    await old;
    expect(model.tree.roots()).toEqual(['urn:R']);
    expect(model.tree.children('urn:R')).toContain('urn:B');
    expect(model.names['urn:B']).toBeDefined();
    expect(model.error).toBeNull();
    model.drop();
  });
  it('an obsolete property locate failure cannot clear a newer successful selection', async () => {
    let release!: (value: unknown) => void;
    let oldSignal: AbortSignal | null | undefined;
    server((call) => {
      const iri = call.url.searchParams.get('iri');
      if (iri === 'urn:A') {
        oldSignal = call.init.signal;
        return new Promise((done) => {
          release = done;
        });
      }
      return hierarchy(
        call,
        iri === 'urn:B'
          ? {
              node: node('urn:B'),
              children: [],
              total: 0,
              ancestors: {
                nodes: [node('urn:R', 1)],
                edges: [['urn:B', 'urn:R']],
                truncated: false,
              },
            }
          : {
              node: iri ? node(iri, 1) : null,
              children: [node(iri ? 'urn:B' : 'urn:R', iri ? 0 : 1)],
            },
      );
    });
    const model = new InferredTree();
    model.reset(view, 'ObjectProperty');
    const old = model.reveal('urn:A');
    await vi.waitFor(() => expect(release).toBeTypeOf('function'));
    await model.reveal('urn:B');
    expect(model.tree.children('urn:R')).toContain('urn:B');
    // The fake transport deliberately completes even if the request was aborted.
    release(
      new Response(JSON.stringify({ message: 'old selected entity is absent' }), { status: 404 }),
    );
    await old;
    expect(model.tree.roots()).toEqual(['urn:R']);
    expect(model.tree.children('urn:R')).toContain('urn:B');
    expect(model.names['urn:B']).toBeDefined();
    expect(model.source?.sourceId).toBe(source.sourceId);
    expect(model.error).toBeNull();
    expect(oldSignal?.aborted).toBe(true);
    model.drop();
  });

  it('an obsolete individual locate failure cannot clear a newer successful membership reveal', async () => {
    let release!: (value: unknown) => void;
    server((call) => {
      if (call.url.searchParams.get('iri') === 'urn:A')
        return new Promise((done) => {
          release = done;
        });
      const members = call.url.searchParams.has('iri') || call.url.searchParams.has('type');
      return {
        commit: 7,
        inferenceSource: source,
        typeBasis: 'materializedNamedTypes',
        groups: [{ type: node('urn:R'), total: members ? null : 1 }],
        individuals: members ? [{ ...node('urn:B'), types: [node('urn:R')] }] : [],
        total: 1,
        next: null,
      };
    });
    const model = new InferredTree();
    model.reset(view, 'NamedIndividual');
    const old = model.reveal('urn:A');
    await vi.waitFor(() => expect(release).toBeTypeOf('function'));
    await model.reveal('urn:B');
    expect(model.individuals.children('@type:urn:R')).toContain('urn:B');
    release(new Response(JSON.stringify({ message: 'old individual is absent' }), { status: 404 }));
    await old;
    expect(model.individuals.roots()).toContain('@type:urn:R');
    expect(model.individuals.children('@type:urn:R')).toContain('urn:B');
    expect(model.individuals.types['urn:B']).toEqual(['urn:R']);
    expect(model.names['urn:B']).toBeDefined();
    expect(model.error).toBeNull();
    model.drop();
  });
  it('reveals an equivalent selected class through bounded kept parents without retargeting selection', async () => {
    const result: ClassificationResult = {
      resultId: 'kept-a',
      classifiedCommit: 7,
      boundStoreId: 'branch',
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
      capabilities: ['namedClassRoots', 'namedClassDirectChildren'],
      justifications: 'unavailable',
    };
    const classRow = (iri: string, children = 0, parents = 1, equivalenceCount = 1) => ({
      iri,
      label: iri,
      labelLang: 'en',
      children,
      parents,
      equivalenceCount,
    });
    const calls = server((call) => {
      const q = call.url.searchParams;
      const part = q.get('part') as 'roots' | 'children' | 'parents';
      const about = q.get('about');
      const root = 'urn:R';
      const canonical = about === 'urn:Alias' ? 'urn:A' : about;
      const rows =
        part === 'roots'
          ? [classRow(root, 1)]
          : part === 'children' && about === root
            ? [classRow('urn:A', 0, 1, 2)]
            : part === 'parents' && canonical === 'urn:A'
              ? [classRow(root, 1)]
              : [];
      const reply: KeptClassification = {
        commit: 7,
        status: 'available',
        result,
        reason: null,
        activity: null,
        staleCommit: null,
        outcome: {
          tier: 'el',
          partial: false,
          complete: true,
          consistency: 'consistent',
          excludedCount: 0,
          incompleteCount: 0,
        },
        entity: about
          ? {
              iri: about,
              present: true,
              representative: canonical,
              unsatisfiable: false,
              children: canonical === root ? 1 : 0,
              parents: 1,
              equivalenceCount: canonical === 'urn:A' ? 2 : 1,
            }
          : null,
        page: {
          part,
          about,
          offset: Number(q.get('offset') ?? 0),
          limit: Number(q.get('limit')),
          total: rows.length,
          items: rows,
          nextOffset: null,
        },
      };
      return reply;
    });
    const pins = new InferredPins('revision-seven');
    const model = new InferredTree(
      () => true,
      (r) => pins.acceptClassification(r.resultId),
    );
    model.reset(view, 'Class', false, pins);
    const ancestors = await model.reveal('urn:Alias');
    expect(model.canonical('urn:Alias')).toBe('urn:A');
    expect(ancestors).toContain('urn:R');
    expect(model.tree.children('urn:R')).toContain('urn:A');
    expect(model.reply?.entity?.iri).not.toBeNull();
    for (const call of calls) {
      expect(call.init.method ?? 'GET').toBe('GET');
      expect(Number(call.url.searchParams.get('limit'))).toBeLessThanOrEqual(100);
      expect(call.url.searchParams.get('at')).toBe('commit:7');
      if (call.url.searchParams.get('part') !== 'roots')
        expect(call.url.searchParams.get('resultId')).toBe('kept-a');
    }
    model.drop();
  });
  it('reads a bounded property level and follows only the lexical producer-pinned cursor', async () => {
    const calls = server((call) => {
      const after = call.url.searchParams.get('after');
      return hierarchy(
        call,
        after
          ? { children: [node('urn:p100')], offset: 100, total: 101 }
          : {
              children: Array.from({ length: 100 }, (_, i) =>
                node(`urn:p${String(i).padStart(3, '0')}`),
              ),
              total: 101,
              nextAfter: 'urn:p099',
              next: 100,
            },
      );
    });
    const model = new InferredTree();
    model.reset(view, 'ObjectProperty');
    await model.tree.load(null);
    expect(model.tree.roots()).toHaveLength(100);
    await model.tree.loadMore(null);
    expect(model.tree.roots()).toHaveLength(101);
    expect(calls).toHaveLength(2);
    expect(calls[1].url.searchParams.get('sourceId')).toBe(source.sourceId);
    expect(calls[1].url.searchParams.get('after')).toBe('urn:p099');
    for (const { url, init } of calls) {
      expect(init.method ?? 'GET').toBe('GET');
      expect(init.cache).toBe('no-store');
      expect(url.searchParams.get('at')).toBe('commit:7');
      expect(url.searchParams.get('branch')).toBe('dev');
      expect(url.searchParams.get('inferences')).toBe('true');
      expect(url.searchParams.has('offset')).toBe(false);
    }
    model.drop();
  });
  it('discards all assembled nodes when a later level changes producer', async () => {
    server((call) =>
      hierarchy(
        call,
        call.url.searchParams.has('iri')
          ? {
              inferenceSource: { ...source, sourceId: `sha256:${'b'.repeat(64)}` },
            }
          : {},
      ),
    );
    const model = new InferredTree();
    model.reset(view, 'DataProperty');
    await model.tree.load(null);
    expect(model.tree.roots()).toEqual(['urn:r']);
    await model.tree.load('urn:r');
    expect(model.tree.roots()).toEqual([]);
    expect(model.names).toEqual({});
    expect(model.error).toMatch(/source changed/);
    model.drop();
  });
  it('refuses a frame from a different producer than the visible hierarchy', async () => {
    const pins = new InferredPins('revision-seven');
    pins.acceptMaterialization(source);
    const calls = server(() =>
      term({ inferenceSource: { ...source, sourceId: `sha256:${'b'.repeat(64)}` } }),
    );
    const frame = new MaterializedFrame();
    frame.reset({ ...view, iri: 'urn:p' }, pins);
    await vi.waitFor(() => expect(frame.loading).toBe(false));
    expect(frame.page).toBeNull();
    expect(frame.error).toMatch(/source changed/);
    expect(calls[0].url.searchParams.get('sourceId')).toBe(source.sourceId);
    expect(calls[0].init.method ?? 'GET').toBe('GET');
    frame.drop();
  });
  it('drops a delayed old frame when entity and commit change', async () => {
    let release!: (value: unknown) => void;
    server((call) =>
      call.url.searchParams.get('iri') === 'urn:p'
        ? new Promise((done) => {
            release = done;
          })
        : term({ iri: 'urn:q', commit: 8, inferenceSource: { ...source, commit: 8 } }),
    );
    const frame = new MaterializedFrame();
    frame.reset({ ...view, iri: 'urn:p' });
    frame.reset({ ...view, commit: 8, iri: 'urn:q' });
    await vi.waitFor(() => expect(frame.page?.iri).toBe('urn:q'));
    release(term());
    await Promise.resolve();
    expect(frame.page?.iri).toBe('urn:q');
    expect(frame.page?.commit).toBe(8);
    frame.drop();
  });
  it.each(['unavailable', 'running'] as const)(
    'preserves the %s root cause while a current selection awaits its roots',
    async (status) => {
      const calls = server(() => ({
        commit: 7,
        status,
        reason: status === 'unavailable' ? 'notKept' : null,
        result: null,
        outcome: null,
        entity: null,
        activity: null,
        page: null,
        staleCommit: null,
      }));
      const model = new InferredTree();
      model.reset(view, 'Class');
      await model.reveal('urn:selected');
      expect(model.reply?.status).toBe(status);
      expect(model.tree.roots()).toEqual([]);
      expect(model.error).toMatch(
        status === 'unavailable' ? /No matching kept classification/ : /Classification is running/,
      );
      expect(model.error).not.toContain('source changed');
      expect(calls).toHaveLength(1);
      expect(calls[0].url.searchParams.get('part')).toBe('roots');
      expect(calls[0].init.method ?? 'GET').toBe('GET');
      model.refresh();
      await model.tree.load(null);
      expect(calls).toHaveLength(2);
      expect(model.reply?.status).toBe(status);
      expect(model.error).toMatch(
        status === 'unavailable' ? /No matching kept classification/ : /Classification is running/,
      );
      expect(calls.every((call) => (call.init.method ?? 'GET') === 'GET')).toBe(true);
      model.drop();
    },
  );
  it('reports unavailable kept state without POST or full-taxonomy fallback', async () => {
    const calls = server(() => ({
      commit: 7,
      status: 'unavailable',
      reason: 'notKept',
      result: null,
      outcome: null,
      entity: null,
      activity: null,
      page: null,
      staleCommit: null,
    }));
    const model = new InferredTree();
    model.reset(view, 'Class');
    await model.tree.load(null);
    expect(model.reply?.status).toBe('unavailable');
    expect(model.tree.roots()).toEqual([]);
    expect(model.error).toMatch(/No matching kept classification/);
    expect(calls).toHaveLength(1);
    expect(calls[0].url.searchParams.get('part')).toBe('roots');
    expect(calls[0].init.method ?? 'GET').toBe('GET');
    model.drop();
  });
});
