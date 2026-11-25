import { describe, expect, it } from 'vitest';
import ox from 'oxigraph';
import { handleOntology, UNI_PREFIXES, UNI_TURTLE } from '../../mock/ontology.mjs';

function makeDataset() {
  const store = new ox.Store();
  store.load(UNI_TURTLE, { format: 'text/turtle' });
  const ds = { id: 'uni', name: 'uni', branchName: 'main', store, prefixes: UNI_PREFIXES };
  const datasets = new Map([['uni', ds]]);
  return {
    datasets,
    ctx: {
      datasets,
      send(res: { status: number; body: unknown }, status: number, body: unknown) {
        res.status = status;
        res.body = body;
      },
      fail(res: { status: number; body: unknown }, status: number, error: string, extra = {}) {
        res.status = status;
        res.body = { error, ...extra };
      },
      readBody(req: { body?: string }) {
        return Promise.resolve(req.body ?? '');
      },
      headCommit: () => ({ seq: 1 }),
      quadSet: () => new Set(),
      commitWrite: () => ({ commit: { seq: 2 } }),
      allCommits: () => [{ seq: 1 }],
    },
  };
}

async function call(
  ctx: ReturnType<typeof makeDataset>['ctx'],
  method: string,
  path: string,
  body?: unknown,
) {
  const url = new URL(path, 'http://localhost');
  const seg = url.pathname.split('/').filter(Boolean);
  const res = {
    status: 0,
    body: null as Record<string, unknown> | null,
    headers: {} as Record<string, string>,
    setHeader(k: string, v: string) {
      this.headers[k] = v;
    },
  };
  const handled = await handleOntology(
    { method, headers: {}, body: body != null ? JSON.stringify(body) : '' },
    res,
    url,
    seg,
    ctx,
  );
  expect(handled).toBe(true);
  return res;
}

describe('mock ontology partial classification', () => {
  it('returns a partial disjunctive answer with unknown consistency under a short deadline and does not retain it', async () => {
    const { ctx } = makeDataset();
    const res = await call(ctx, 'POST', '/$/ontology/uni/classify?timeout=0.2', {
      graph: 'default',
      imports: true,
      nodes: true,
      partial: true,
      expansion: 'eager',
    });
    expect(res.status).toBe(200);
    const body = res.body as Record<string, unknown>;
    expect(body.tier).toBe('disjunctive');
    expect(body.partial).toBe(true);
    expect(body.complete).toBe(false);
    expect(body.completeForTaken).toBe(false);
    expect(body.consistency).toBe('unknown');
    expect(body.consistent).toBe(true);
    expect(body.incompleteCount).toBe(body.classes);
    expect((body.incomplete as string[]).length).toBe(body.classes);

    const kept = await call(ctx, 'GET', '/$/ontology/uni/classification?graph=default&at=commit:1');
    expect(kept.status).toBe(200);
    expect(kept.body?.status).toBe('unavailable');
  });

  it('returns a partial consistent answer with a subset of open classes under a longer deadline and supports frame projection', async () => {
    const { ctx } = makeDataset();
    const res = await call(ctx, 'POST', '/$/ontology/uni/classify?timeout=2', {
      graph: 'default',
      imports: true,
      nodes: true,
      partial: true,
      expansion: 'cautious',
    });
    expect(res.status).toBe(200);
    const body = res.body as Record<string, unknown>;
    expect(body.tier).toBe('disjunctive');
    expect(body.partial).toBe(true);
    expect(body.consistency).toBe('consistent');
    const incomplete = body.incomplete as string[];
    expect(incomplete.length).toBeGreaterThan(0);
    expect(incomplete.length).toBeLessThan(Number(body.classes));

    const openClass = incomplete[0];
    const frameRes = await call(ctx, 'POST', '/$/ontology/uni/classify?timeout=2', {
      graph: 'default',
      imports: true,
      projection: 'frame',
      about: openClass,
      partial: true,
      expansion: 'eager',
    });
    expect(frameRes.status).toBe(200);
    const frame = (frameRes.body as { frame: Record<string, unknown> }).frame;
    expect(frame.incomplete).toBe(true);
    expect(frame.outcome).toMatchObject({
      tier: 'disjunctive',
      partial: true,
      complete: false,
      consistency: 'consistent',
      incompleteCount: incomplete.length,
    });
    expect(frame.retention).toEqual({ kept: false, resultId: null });
    for (const row of (frame.parents as { items: Record<string, unknown>[] }).items)
      expect('incomplete' in row).toBe(false);
    for (const row of (frame.equivalents as { items: Record<string, unknown>[] }).items)
      expect('incomplete' in row).toBe(false);
  });

  it('keeps inputFingerprint independent of partial/expansion and omits row-level incomplete on kept pages', async () => {
    const { ctx } = makeDataset();
    const full = await call(ctx, 'POST', '/$/ontology/uni/classify', {
      graph: 'default',
      imports: true,
      nodes: true,
    });
    const partial = await call(ctx, 'POST', '/$/ontology/uni/classify?timeout=2', {
      graph: 'default',
      imports: true,
      nodes: true,
      partial: true,
      expansion: 'eager',
    });
    expect(
      (partial.body?.classificationSource as { inputFingerprint: string }).inputFingerprint,
    ).toBe((full.body?.classificationSource as { inputFingerprint: string }).inputFingerprint);

    const roots = await call(
      ctx,
      'GET',
      '/$/ontology/uni/classification?graph=default&at=commit:1&part=roots',
    );
    expect(roots.status).toBe(200);
    expect(roots.body?.status).toBe('available');
    const items = (roots.body?.page as { items: Record<string, unknown>[] }).items;
    expect(items.length).toBeGreaterThan(0);
    for (const row of items) expect('incomplete' in row).toBe(false);
  });

  it('rejects an invalid expansion value', async () => {
    const { ctx } = makeDataset();
    const res = await call(ctx, 'POST', '/$/ontology/uni/classify', {
      graph: 'default',
      expansion: 'invalid',
    });
    expect(res.status).toBe(400);
  });
});
