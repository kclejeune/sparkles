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
  it('names one graph for each ontology graph, and the default graph has its own', () => {
    const D = 'urn:x-sparkles:discussions:';
    expect(discussionGraph(G)).toBe(D + G);
    expect(discussionGraph(null)).toBe(DEFAULT_DISCUSSIONS);
    // graphs that differ only in a trailing `/` or `#` are distinct ontologies
    const names = [G, `${G}/`, `${G}#`].map(discussionGraph);
    expect(new Set(names).size).toBe(3);
    expect(isDiscussionGraph(D + G)).toBe(true);
    expect(isDiscussionGraph(DEFAULT_DISCUSSIONS)).toBe(true);
    expect(isDiscussionGraph(G)).toBe(false);
    // an ontology graph whose IRI happens to end in /discussions is not one
    expect(isDiscussionGraph(`${G}/discussions`)).toBe(false);
    expect(isDiscussionGraph(null)).toBe(false);
  });

  it('names a valid graph for IPv6 hosts and percent-encoded names, one for each', () => {
    const store = new ox.Store();
    const graphs = [
      'http://[::1]/onto',
      'http://example.org/a%20b',
      'http://example.org/a b'.replace(' ', '%2520'),
    ];
    graphs.forEach((g, i) =>
      store.update(
        newThreadUpdate(g, {
          id: `urn:uuid:${i}`,
          text: 'Hi',
          user: 'bo',
          now: '2026-11-01T10:00:00Z',
          entity: EX + 'A',
        }),
      ),
    );
    graphs.forEach((g, i) => {
      expect(isDiscussionGraph(discussionGraph(g))).toBe(true);
      expect(read(store, null, g).threads.map((t) => t.id)).toEqual([`urn:uuid:${i}`]);
    });
    expect(new Set(graphs.map(discussionGraph)).size).toBe(graphs.length);
  });

  it('keeps the threads of graphs that differ only in a trailing / apart', () => {
    const store = new ox.Store();
    const c = { text: 'Hi', user: 'bo', now: '2026-11-01T10:00:00Z', entity: EX + 'A' };
    store.update(newThreadUpdate(G, { ...c, id: 'urn:uuid:1' }));
    store.update(newThreadUpdate(`${G}/`, { ...c, id: 'urn:uuid:2' }));
    expect(read(store, null, G).threads.map((t) => t.id)).toEqual(['urn:uuid:1']);
    expect(read(store, null, `${G}/`).threads.map((t) => t.id)).toEqual(['urn:uuid:2']);
    expect(read(store, null, `${G}#`).threads).toEqual([]);
  });
});

describe('mentions', () => {
  it('reads @names once each, not e-mail addresses', () => {
    expect(mentionsOf('@ana and @bo.k, again @ana; mail x@y.org')).toEqual(['ana', 'bo.k']);
    // one-character names and principals that are e-mail addresses
    expect(mentionsOf('@a and @alice@example.org.')).toEqual(['a', 'alice@example.org']);
    expect(textParts('@alice@example.org.')).toEqual([
      { text: '@alice@example.org', mention: true },
      { text: '.', mention: false },
    ]);
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
    expect([...graphs]).toEqual([discussionGraph(G)]);
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

  it('shows a creator IRI that is not valid percent-encoding as it is', () => {
    const rows = [
      {
        a: { value: 'urn:uuid:bad' },
        motivation: { value: 'http://www.w3.org/ns/oa#commenting' },
        target: { value: EX + 'A' },
        text: { value: 'Imported.' },
        creator: { value: 'urn:x-sparkles:user:bad%FF' },
        created: { value: '2026-11-01T10:00:00Z' },
      },
    ];
    expect(parseThreads(rows).threads[0].creator).toBe('urn:x-sparkles:user:bad%FF');
  });

  it('refuses empty and oversized comments', () => {
    const base = { id: t1, entity: EX + 'A', user: 'bo', now: '2026-11-01T10:00:00Z' };
    expect(() => newThreadUpdate(G, { ...base, text: '  ' })).toThrow(/needs some text/);
    expect(() => newThreadUpdate(G, { ...base, text: 'x'.repeat(10_001) })).toThrow(/at most/);
  });
});
