import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Task, Term } from './api';
import {
  ALGOS,
  drawnPredicates,
  drawnQuery,
  NoViewRuns,
  pickLevel,
  readRows,
  runParams,
  runProjection,
  summaryText,
  type ProjectionContext,
} from './algo';

const EX = 'http://example.org/';
const iri = (v: string): Term => ({ type: 'uri', value: EX + v });
const lit = (v: string | number): Term => ({ type: 'literal', value: String(v) });

describe('drawn-graph query (§3.2)', () => {
  it('writes the drawn nodes into both VALUES blocks with the outputs and parameters', () => {
    const q = drawnQuery(ALGOS.pageRank, [`${EX}a`, `${EX}b`], {
      dampingFactor: 0.85,
      maxIterations: 20,
    });
    expect(q).toContain('PREFIX algo: <urn:x-sparkles:algo#>');
    expect(q).toContain('SELECT ?node ?score ?summary WHERE');
    expect(q).toContain('SERVICE algo:pageRank');
    expect(q).toContain('algo:start ?s ; algo:end ?o ; algo:node ?node ; algo:score ?score');
    expect(q).toContain('algo:dampingFactor 0.85 ; algo:maxIterations 20');
    expect(q).toContain(`VALUES ?s { <${EX}a> <${EX}b> }`);
    expect(q).toContain(`VALUES ?o { <${EX}a> <${EX}b> }`);
    expect(q).toContain('?s ?p ?o .');
  });

  it('asks a hierarchical run for its level and BFS for its start node', () => {
    expect(drawnQuery(ALGOS.leiden, [`${EX}a`], runParams(ALGOS.leiden))).toContain(
      'SELECT ?node ?community ?level ?summary',
    );
    const p = runParams(ALGOS.bfs, {}, `${EX}a`);
    expect(drawnQuery(ALGOS.bfs, [`${EX}a`], p)).toContain(`algo:source <${EX}a>`);
  });

  it('leaves out unset parameters and keeps the ones a person changed', () => {
    expect(runParams(ALGOS.betweenness)).toEqual({ seed: 42 });
    expect(runParams(ALGOS.betweenness, { samples: 50 })).toEqual({ samples: 50, seed: 42 });
    expect(runParams(ALGOS.leiden, { seed: 43 })).toEqual({ resolution: 1, seed: 43 });
  });
});

describe('rows', () => {
  it('reads scores and the summary', () => {
    const r = readRows(ALGOS.pageRank, [
      { node: iri('a'), score: lit(0.5), summary: lit('{"iterations":12,"converged":true}') },
      { node: iri('b'), score: lit(0.1), summary: lit('{"iterations":12,"converged":true}') },
    ]);
    expect(r.values.get(`${EX}a`)).toBe(0.5);
    expect(r.summary).toEqual({ iterations: 12, converged: true });
    expect(summaryText(r.summary)).toBe('12 iterations, converged');
  });

  it('picks the chosen level of a hierarchical run, or the last one', () => {
    const rows = [
      { node: iri('a'), community: lit(1), level: lit(0), communitySize: lit(2) },
      { node: iri('b'), community: lit(1), level: lit(0), communitySize: lit(2) },
      { node: iri('a'), community: lit(9), level: lit(1), communitySize: lit(40) },
      { node: iri('b'), community: lit(8), level: lit(1), communitySize: lit(3) },
    ];
    const last = readRows(ALGOS.louvain, rows);
    expect(last.levels).toEqual([0, 1]);
    expect(last.values.get(`${EX}a`)).toBe('9');
    expect(last.sizes.get('9')).toBe(40);
    const first = readRows(ALGOS.louvain, rows, 0);
    expect(first.values.get(`${EX}b`)).toBe('1');
    expect(pickLevel([0, 1, 2], 7)).toBe(2);
    expect(summaryText({ modularity: 0.4123, communities: 3, levels: 2 })).toBe(
      'modularity 0.41, 3 in all',
    );
  });

  it('takes the predicates of the drawn edges, without variables', () => {
    const e = (p: string) => ({
      triple: [iri('a'), { type: 'uri', value: p }, iri('b')] as [Term, Term, Term],
    });
    expect(
      drawnPredicates([e(`${EX}knows`), e(`${EX}knows`), e('urn:var:p'), e(`${EX}age`)]),
    ).toEqual([`${EX}age`, `${EX}knows`]);
  });
});

describe('projection runs (§3.2)', () => {
  const answer = (nodes: readonly string[], cached: boolean, score = (n: string) => n.length) => ({
    answer: {
      head: { vars: ['node', 'score'] },
      results: {
        bindings: nodes.map((n) => ({
          node: { type: 'uri' as const, value: n },
          score: { type: 'literal' as const, value: String(score(n)) },
        })),
      },
      summary: { iterations: 3, converged: true },
      run: { commit: 7, cached, rows: 100 },
    },
  });

  it('starts a kept background run, follows its task, then reads the kept result', async () => {
    const posted: Record<string, unknown>[] = [];
    const seen: Task[] = [];
    let polls = 0;
    const ctx: ProjectionContext = {
      ds: 'ds@dev',
      at: 'commit:7',
      poll: 1,
      onTask: (t) => seen.push(t),
      post: async (path, body) => {
        expect(path).toBe('/$/algo/ds%40dev/run?at=commit%3A7');
        const b = body as Record<string, unknown>;
        posted.push(b);
        if (b.async)
          return {
            task: { id: '5', kind: 'algo-view', state: 'running', progress: 0 } as unknown as Task,
          };
        return answer(b.nodes as string[], true);
      },
      getTask: async () => {
        polls++;
        return (polls < 2
          ? { id: '5', state: 'running', progress: 0.5 }
          : { id: '5', state: 'done', detail: { kept: true } }) as unknown as Task;
      },
    };
    const r = await runProjection(ctx, ALGOS.pageRank, { name: 'people' }, [`${EX}a`], {
      dampingFactor: 0.85,
    });
    expect(posted[0]).toMatchObject({
      algorithm: 'pageRank',
      projection: 'people',
      keep: true,
      async: true,
      nodes: [`${EX}a`],
    });
    expect(posted[1].async).toBeUndefined();
    expect(seen.map((t) => t.state)).toEqual(['running', 'running', 'done']);
    expect(r.values.get(`${EX}a`)).toBe(`${EX}a`.length);
    expect(r.cached).toBe(true);
    expect(r.summary).toEqual({ iterations: 3, converged: true });
  });

  it('reads above 20,000 nodes in chunks and keeps the top of all of them', async () => {
    const iris = Array.from({ length: 45_000 }, (_, i) => `${EX}n${i}`);
    const sizes: number[] = [];
    const ctx: ProjectionContext = {
      ds: 'ds',
      post: async (_p, body) => {
        const b = body as { nodes: string[]; top?: number };
        sizes.push(b.nodes.length);
        expect(b.top).toBe(1000);
        // every chunk answers its own top
        const top = [...b.nodes]
          .sort((x, y) => Number(y.slice(EX.length + 1)) - Number(x.slice(EX.length + 1)))
          .slice(0, b.top);
        return answer(top, false, (n) => Number(n.slice(EX.length + 1)));
      },
    };
    const r = await runProjection(
      ctx,
      ALGOS.pageRank,
      { predicates: [`${EX}knows`] },
      iris,
      {},
      { top: 1000 },
    );
    expect(sizes).toEqual([20_000, 20_000, 5_000]);
    expect(r.values.size).toBe(1000);
    expect(r.values.has(`${EX}n44999`)).toBe(true);
    expect(r.values.has(`${EX}n43999`)).toBe(false);
  });

  it('says so when the run was cancelled or not kept', async () => {
    const run = (end: Partial<Task> & { detail?: unknown }) =>
      runProjection(
        {
          ds: 'ds',
          poll: 1,
          post: async (_p, b) =>
            (b as { async?: boolean }).async
              ? { task: { id: '1', state: 'running' } as unknown as Task }
              : answer([], true),
          getTask: async () => ({ id: '1', ...end }) as unknown as Task,
        },
        ALGOS.pageRank,
        { name: 'p' },
        [`${EX}a`],
        {},
      );
    await expect(run({ state: 'cancelled' })).rejects.toThrow('cancelled');
    await expect(run({ state: 'done', detail: { kept: false } })).rejects.toThrow('no memory');
  });

  afterEach(() => vi.unstubAllGlobals());

  it('turns a server without view-only runs into a reason to disable projection scope', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              error: 'invalid request: unknown field `nodes`, expected one of `algorithm`',
            }),
            {
              status: 400,
              headers: { 'Content-Type': 'application/json' },
            },
          ),
      ),
    );
    await expect(
      runProjection({ ds: 'ds' }, ALGOS.pageRank, { name: 'p' }, [`${EX}a`], {}),
    ).rejects.toBeInstanceOf(NoViewRuns);
  });
});
