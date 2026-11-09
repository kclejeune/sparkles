import { describe, expect, it } from 'vitest';
import ox from 'oxigraph';
import {
  DEFAULT_DISCUSSIONS,
  discussionGraph,
  isDiscussionGraph,
  mentionsOf,
  newThreadUpdate,
  parseThreads,
  replyUpdate,
  resolveUpdate,
  textParts,
  threadsQuery,
} from './discussions';

const EX = 'http://example.org/uni#';
const G = 'http://example.org/uni';

/** Run `threadsQuery` on a store, with the rows shaped as the SPARQL JSON results are. */
function read(store: ox.Store, entity: string | null, graph: string | null = G) {
  const rows = (store.query(threadsQuery(graph, entity)) as Map<string, ox.Term>[]).map((m) =>
    Object.fromEntries([...m].map(([k, t]) => [k, { value: t.value }])),
  );
  return parseThreads(rows);
}

describe('discussion graphs', () => {
  it('sits beside the ontology graph, and the default graph has its own', () => {
    expect(discussionGraph(G)).toBe(`${G}/discussions`);
    expect(discussionGraph(`${G}#`)).toBe(`${G}/discussions`);
    expect(discussionGraph(`${G}/`)).toBe(`${G}/discussions`);
    expect(discussionGraph(null)).toBe(DEFAULT_DISCUSSIONS);
    expect(isDiscussionGraph(`${G}/discussions`)).toBe(true);
    expect(isDiscussionGraph(DEFAULT_DISCUSSIONS)).toBe(true);
    expect(isDiscussionGraph(G)).toBe(false);
    expect(isDiscussionGraph(null)).toBe(false);
  });
});

describe('mentions', () => {
  it('reads @names once each, not e-mail addresses', () => {
    expect(mentionsOf('@ana and @bo.k, again @ana; mail x@y.org')).toEqual(['ana', 'bo.k']);
    expect(textParts('Ask @ana.')).toEqual([
      { text: 'Ask ', mention: false },
      { text: '@ana', mention: true },
      { text: '.', mention: false },
    ]);
  });
});

describe('threads as Web Annotation RDF', () => {
  const store = new ox.Store();
  const t1 = 'urn:uuid:00000000-0000-4000-8000-000000000001';
  const t2 = 'urn:uuid:00000000-0000-4000-8000-000000000002';
  const r1 = 'urn:uuid:00000000-0000-4000-8000-000000000003';
  const t3 = 'urn:uuid:00000000-0000-4000-8000-000000000004';
  store.update(
    newThreadUpdate(G, {
      id: t1,
      entity: EX + 'Professor',
      text: 'Should this be @ana\'s "Lecturer"?',
      user: 'bo',
      now: '2026-11-01T10:00:00Z',
    }),
  );
  store.update(
    newThreadUpdate(G, {
      id: t2,
      entity: EX + 'Professor',
      axiom: 'SubClassOf(uni:Professor uni:Employee)',
      text: 'Too broad.',
      user: 'ana',
      now: '2026-11-01T11:00:00Z',
    }),
  );
  store.update(
    replyUpdate(G, {
      id: r1,
      thread: t1,
      text: 'Agreed.',
      user: 'ana',
      now: '2026-11-01T12:00:00Z',
    }),
  );
  store.update(
    newThreadUpdate(G, {
      id: t3,
      entity: EX + 'Course',
      text: 'Elsewhere.',
      user: 'bo',
      now: '2026-11-01T13:00:00Z',
    }),
  );

  it('writes into the discussion graph only, and never as rdfs:comment', () => {
    const graphs = new Set(store.match().map((q) => q.graph.value));
    expect([...graphs]).toEqual([`${G}/discussions`]);
    expect(
      store.match(null, ox.namedNode('http://www.w3.org/2000/01/rdf-schema#comment')),
    ).toHaveLength(0);
  });

  it('reads the threads about an entity, its axioms included, with their replies', () => {
    const { threads, truncated } = read(store, EX + 'Professor');
    expect(truncated).toBe(false);
    expect(threads.map((t) => t.id)).toEqual([t1, t2]);
    expect(threads[0]).toMatchObject({
      entity: EX + 'Professor',
      axiom: null,
      creator: 'bo',
      text: 'Should this be @ana\'s "Lecturer"?',
      resolved: false,
    });
    expect(threads[0].replies.map((r) => [r.text, r.creator])).toEqual([['Agreed.', 'ana']]);
    expect(threads[1].axiom).toBe('SubClassOf(uni:Professor uni:Employee)');
    expect(threads[1].replies).toEqual([]);
  });

  it('lists every thread of the ontology without an entity', () => {
    expect(read(store, null).threads.map((t) => t.entity)).toEqual([
      EX + 'Professor',
      EX + 'Professor',
      EX + 'Course',
    ]);
    // another ontology graph has its own discussions
    expect(read(store, null, 'http://example.org/other').threads).toEqual([]);
  });

  it('resolves and reopens a thread, recording who resolved it', () => {
    store.update(resolveUpdate(G, t1, true, 'ana', '2026-11-02T09:00:00Z'));
    let t = read(store, EX + 'Professor').threads[0];
    expect([t.resolved, t.resolvedBy]).toEqual([true, 'ana']);
    store.update(resolveUpdate(G, t1, false, 'bo', '2026-11-02T10:00:00Z'));
    t = read(store, EX + 'Professor').threads[0];
    expect([t.resolved, t.resolvedBy]).toEqual([false, null]);
    // one resolved value at a time
    const values = store.match(
      ox.namedNode(t1),
      ox.namedNode('urn:x-sparkles:discussion#resolved'),
    );
    expect(values).toHaveLength(1);
  });

  it('refuses empty and oversized comments', () => {
    const base = { id: t1, entity: EX + 'A', user: 'bo', now: '2026-11-01T10:00:00Z' };
    expect(() => newThreadUpdate(G, { ...base, text: '  ' })).toThrow(/needs some text/);
    expect(() => newThreadUpdate(G, { ...base, text: 'x'.repeat(10_001) })).toThrow(/at most/);
  });
});
