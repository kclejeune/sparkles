import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetchIndividuals } from './browse';
import {
  IndividualTree,
  groupKey,
  type IndividualPage,
  type IndividualQuery,
} from './individual-tree.svelte';
import type { Name } from './browse';

const name = (iri: string): Name => ({
  iri,
  label: iri,
  labelLang: null,
  shortForm: null,
  deprecated: false,
  kinds: [],
});
const A = 'http://example.org/A';
const B = 'http://example.org/B';
const key = groupKey(A);
const answer = (p: Partial<IndividualPage> = {}): IndividualPage => ({
  commit: 42,
  groups: [],
  individuals: [],
  total: 0,
  next: null,
  ...p,
});
const member = (iri: string) => ({ ...name(iri), types: [name(A)] });

describe('paged Individuals tab', () => {
  it('opens on buckets, expands one bounded page, and continues at its pinned commit', async () => {
    const reads: IndividualQuery[] = [];
    const t = new IndividualTree(async (q) => {
      reads.push(q);
      if (!q.type) return answer({ groups: [{ type: name(A), total: 250 }], total: 1 });
      return answer({
        individuals: [member(q.after ? 'http://example.org/i101' : 'http://example.org/i001')],
        total: 250,
        next: q.after ? null : 'http://example.org/i100',
      });
    });
    await t.load(null);
    expect(reads).toEqual([{ limit: 100 }]);
    expect(t.roots()).toEqual([key]);
    expect(t.children(key)).toEqual([]);
    await Promise.all([t.load(key), t.load(key)]);
    expect(reads).toHaveLength(2);
    expect(reads[1]).toEqual({ type: A, at: 'commit:42', limit: 100 });
    expect(t.clippedOf(key)).toEqual({ shown: 1, total: 250 });
    await t.loadMore(key);
    expect(reads[2]).toEqual({
      type: A,
      at: 'commit:42',
      after: 'http://example.org/i100',
      limit: 100,
    });
    expect(t.children(key)).toEqual(['http://example.org/i001', 'http://example.org/i101']);
    expect(t.clippedOf(key)).toBeNull();
  });

  it('reveals a far member in every type without walking preceding pages', async () => {
    const reads: IndividualQuery[] = [];
    const far = 'http://example.org/z999999';
    const t = new IndividualTree(async (q) => {
      reads.push(q);
      if (q.iri)
        return answer({
          groups: [
            { type: name(A), total: 0 },
            { type: name(B), total: 0 },
          ],
          individuals: [{ ...member(far), types: [name(A), name(B)] }],
          total: 1,
        });
      if (!q.type)
        return answer({ groups: [{ type: name(A), total: 1_000_000 }], next: A, total: 2 });
      return answer({
        individuals: [member('http://example.org/a')],
        next: 'http://example.org/a',
        total: 1_000_000,
      });
    });
    expect(new Set(await t.reveal(far))).toEqual(new Set([key, groupKey(B)]));
    expect(t.children(key)).toContain(far);
    expect(t.children(groupKey(B))).toContain(far);
    expect(reads).toHaveLength(4);
    expect(reads.every((r) => r.after == null)).toBe(true);
    // A seek does not alter the server's normal continuation.
    await t.loadMore(key);
    expect(reads[4].after).toBe('http://example.org/a');
  });

  it('aborts resets and drops stale rows, labels, errors and pending cleanup', async () => {
    let resolve!: (p: IndividualPage) => void;
    let oldSignal!: AbortSignal;
    const names: string[] = [];
    let count = 0;
    const t = new IndividualTree(
      (_, signal) => {
        count++;
        if (count === 1) {
          oldSignal = signal;
          return new Promise((r) => {
            resolve = r;
          });
        }
        return Promise.resolve(
          answer({ groups: [{ type: null, total: 1 }], total: 1, commit: 99 }),
        );
      },
      (nodes) => names.push(...nodes.map((n) => n.iri)),
    );
    const old = t.load(null);
    t.drop();
    await t.load(null);
    expect(oldSignal.aborted).toBe(true);
    resolve(answer({ groups: [{ type: name(A), total: 1000 }], total: 1 }));
    await old;
    expect(t.commit).toBe(99);
    expect(t.roots()).toEqual([groupKey(null)]);
    expect(names).toEqual([]);
    expect(t.loading).toBe(false);
  });

  it('rejects a continuation from another revision', async () => {
    const t = new IndividualTree(async (q) =>
      q.type
        ? answer({ commit: 43, individuals: [member('http://example.org/wrong')] })
        : answer({ groups: [{ type: name(A), total: 1 }], total: 1 }),
    );
    await t.load(null);
    await t.load(key);
    expect(t.children(key)).toEqual([]);
    expect(t.error).toContain('changed revision');
  });
});

describe('individual page transport', () => {
  afterEach(() => vi.unstubAllGlobals());
  it('preserves an empty continuation, graph/import scope, revision, languages and cancellation', async () => {
    const fetch = vi.fn(async () => Response.json(answer()));
    vi.stubGlobal('fetch', fetch);
    const ctl = new AbortController();
    await fetchIndividuals(
      'test@dev',
      { graph: 'http://example.org/g', imports: false },
      { after: '', at: 'commit:42', limit: 100, lang: ['fr', 'en'] },
      ctl.signal,
    );
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    const query = new URL(url, 'http://localhost').searchParams;
    expect(url).toContain('/$/ontology/test/individuals?');
    expect(query.get('branch')).toBe('dev');
    expect(query.get('graph')).toBe('http://example.org/g');
    expect(query.get('imports')).toBe('false');
    expect(query.has('after')).toBe(true);
    expect(query.get('after')).toBe('');
    expect(query.get('at')).toBe('commit:42');
    expect(query.get('lang')).toBe('fr,en');
    expect(init.signal).toBe(ctl.signal);
  });
});
