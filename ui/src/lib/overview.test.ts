import { describe as group, expect, it } from 'vitest';
import type { ClassProfile, GroupsAnswer } from './api';
import {
  chooseRenderer,
  communityLevel,
  fallbackNode,
  isReports,
  classLevel,
  describe,
  hash01,
  nodeRadius,
  seed,
  visible,
  WEBGL_ABOVE,
} from './overview';

const EX = 'http://ex/';

function profile(cls: string, instances: number, props: ClassProfile['properties'] = []) {
  return { class: cls, builtin: false, instances, properties: props, incoming: [] };
}

function prop(predicate: string, objectClasses: { class: string; triples: number }[]) {
  return {
    predicate,
    instances: 1,
    triples: objectClasses.reduce((a, b) => a + b.triples, 0),
    minPerInstance: 1,
    maxPerInstance: 1,
    objects: {},
    objectClasses,
  };
}

// A4: three people work for one organization
const profiles: ClassProfile[] = [
  profile(`${EX}Person`, 3, [
    prop(`${EX}worksFor`, [{ class: `${EX}Org`, triples: 3 }]),
    prop(`${EX}knows`, [{ class: `${EX}Person`, triples: 2 }]),
  ]),
  profile(`${EX}Org`, 1),
  { ...profile('http://www.w3.org/2000/01/rdf-schema#Class', 2), builtin: true },
  profile(`${EX}Empty`, 0),
];

function answer(over: Partial<GroupsAnswer>): GroupsAnswer {
  return {
    commit: 1,
    within: { class: `${EX}Person`, size: 3 },
    grouping: { kind: 'communities', predicate: `${EX}community` },
    communities: true,
    groups: [],
    edges: [],
    truncated: false,
    ...over,
  };
}

group('the class level', () => {
  it('draws one node per class and one edge per class pair (A4)', () => {
    const g = classLevel(profiles, { builtins: false });
    expect(g.nodes.map((n) => [n.key, n.size])).toEqual([
      [`c:${EX}Person`, 3],
      [`c:${EX}Org`, 1],
    ]);
    expect(g.edges).toEqual([
      {
        key: `c:${EX}Person\u0001c:${EX}Org`,
        source: `c:${EX}Person`,
        target: `c:${EX}Org`,
        count: 3,
        predicates: [{ iri: `${EX}worksFor`, count: 3 }],
      },
    ]);
    // links among a class's own instances are a count, not a loop
    expect(g.nodes[0].internal).toBe(2);
  });

  it('keeps or hides the built-in classes', () => {
    expect(classLevel(profiles).nodes).toHaveLength(3);
    expect(classLevel(profiles, { builtins: false }).nodes).toHaveLength(2);
  });
});

group('semantic zoom', () => {
  const base = classLevel(profiles, { builtins: false });
  const c1 = 'g1.one';
  const c2 = 'g1.two';
  const opened = answer({
    groups: [
      { id: c1, label: 'Community 1', size: 2, top: [] },
      { id: c2, label: 'Community 2', size: 1, top: [] },
    ],
    edges: [
      { source: `g:${c1}`, target: `c:${EX}Org`, predicate: `${EX}worksFor`, count: 2 },
      { source: `g:${c2}`, target: `c:${EX}Org`, predicate: `${EX}worksFor`, count: 1 },
      { source: `g:${c1}`, target: `g:${c2}`, predicate: `${EX}knows`, count: 1 },
      { source: `g:${c1}`, target: `g:${c1}`, predicate: `${EX}knows`, count: 1 },
    ],
  });

  it('replaces an opened class with its groups and keeps the other classes (A5)', () => {
    const g = visible(base, [{ key: `c:${EX}Person`, label: 'Person', answer: opened }]);
    expect(g.nodes.map((n) => n.key).sort()).toEqual([`c:${EX}Org`, `g:${c1}`, `g:${c2}`].sort());
    const groups = g.nodes.filter((n) => n.kind === 'group');
    expect(groups.reduce((a, n) => a + n.size, 0)).toBe(3);
    const toOrg = g.edges.filter((e) => e.target === `c:${EX}Org`);
    expect(toOrg.map((e) => e.count).sort()).toEqual([1, 2]);
    // the class-level edge from the opened class is gone
    expect(g.edges.some((e) => e.source === `c:${EX}Person`)).toBe(false);
    expect(g.nodes.find((n) => n.key === `g:${c1}`)?.internal).toBe(1);
    // the base is not changed
    expect(base.nodes.find((n) => n.key === `c:${EX}Org`)?.internal).toBeUndefined();
  });

  it('opens a group into members, with edges to the sibling group (A5)', () => {
    const members = answer({
      within: { class: `${EX}Person`, group: c1, size: 2 },
      grouping: { kind: 'members', predicate: null },
      communities: false,
      members: [{ iri: `${EX}a`, label: 'A' }, { iri: `${EX}b` }],
      edges: [
        { source: `m:${EX}a`, target: `m:${EX}b`, predicate: `${EX}knows`, count: 1 },
        { source: `m:${EX}a`, target: `g:${c2}`, predicate: `${EX}knows`, count: 1 },
        { source: `m:${EX}b`, target: `c:${EX}Org`, predicate: `${EX}worksFor`, count: 1 },
      ],
    });
    const path = [
      { key: `c:${EX}Person`, label: 'Person', answer: opened },
      { key: `g:${c1}`, label: 'Community 1', answer: members },
    ];
    const g = visible(base, path, (i) => i.replace(EX, 'ex:'));
    expect(g.nodes.map((n) => n.key).sort()).toEqual(
      [`c:${EX}Org`, `g:${c2}`, `m:${EX}a`, `m:${EX}b`].sort(),
    );
    expect(g.nodes.find((n) => n.key === `m:${EX}b`)?.label).toBe('ex:b');
    expect(g.edges.find((e) => e.source === `m:${EX}a` && e.target === `g:${c2}`)?.count).toBe(1);
    // the edges of the closed group c1 are not drawn
    expect(g.edges.some((e) => e.source === `g:${c1}`)).toBe(false);
    // closing (A6): the path without its last level gives the groups again
    const back = visible(base, path.slice(0, 1));
    expect(back.nodes.map((n) => n.key).sort()).toEqual(
      [`c:${EX}Org`, `g:${c1}`, `g:${c2}`].sort(),
    );
    expect(describe(g, path, String)).toBe('Community 1 opened into 2 members.');
  });

  it('announces the class level', () => {
    expect(describe(base, [], String)).toBe('Class overview: 2 classes, 1 predicate.');
  });
});

group('positions and sizes', () => {
  it('seeds the same position for the same key, around an anchor', () => {
    expect(seed('a')).toEqual(seed('a'));
    expect(seed('a')).not.toEqual(seed('b'));
    const p = seed('x', { x: 100, y: 100 }, 10);
    expect(Math.hypot(p.x - 100, p.y - 100)).toBeLessThanOrEqual(10);
    expect(hash01('k')).toBeGreaterThanOrEqual(0);
    expect(hash01('k')).toBeLessThan(1);
  });

  it('sizes nodes by the log of their instances', () => {
    expect(nodeRadius(1000, 1000, 'class')).toBe(24);
    expect(nodeRadius(0, 1000, 'class')).toBe(5);
    expect(nodeRadius(10, 1000, 'class')).toBeLessThan(nodeRadius(100, 1000, 'class'));
    expect(nodeRadius(1, 1, 'member')).toBe(4);
  });
});

group('the renderer', () => {
  it('chooses WebGL above the threshold, unless overridden or unavailable (A1, A9)', () => {
    expect(chooseRenderer(WEBGL_ABOVE, null, true).renderer).toBe('cytoscape');
    expect(chooseRenderer(WEBGL_ABOVE + 1, null, true).renderer).toBe('webgl');
    expect(chooseRenderer(20000, 'cytoscape', true).renderer).toBe('cytoscape');
    expect(chooseRenderer(10, 'webgl', true).renderer).toBe('webgl');
    expect(chooseRenderer(20000, 'webgl', false)).toEqual({
      renderer: 'cytoscape',
      reason: 'WebGL is not available in this browser',
    });
  });
});

/** The GraphRAG communities of the whole dataset: two teams, the first with two below. */
function reports(over: Partial<GroupsAnswer> = {}): GroupsAnswer {
  return {
    commit: 4,
    available: true,
    within: { reports: { graph: 'urn:x-sparkles:graphrag:reports', level: 0 }, size: 9 },
    grouping: { kind: 'reports', predicate: null, level: 0 },
    communities: false,
    groups: [
      {
        id: 'r1.a',
        label: 'Team A',
        size: 6,
        top: [],
        community: 'urn:c:A',
        level: 0,
        children: 2,
        report: { iri: 'urn:r:A', title: 'Team A', status: 'reviewed' },
      },
      {
        id: 'r1.b',
        label: 'Team B',
        size: 3,
        top: [],
        community: 'urn:c:B',
        level: 0,
        children: 0,
        report: null,
      },
    ],
    edges: [
      { source: 'g:r1.a', target: 'g:r1.a', predicate: `${EX}knows`, count: 5 },
      { source: 'g:r1.a', target: 'g:r1.b', predicate: `${EX}knows`, count: 1 },
    ],
    truncated: false,
    ...over,
  };
}

group('the GraphRAG communities', () => {
  it('draws one node per community, sized by members, coloured by its own key', () => {
    const a = reports();
    expect(isReports(a)).toBe(true);
    expect(isReports(answer({}))).toBe(false);
    const g = communityLevel(a, (i) => i);
    expect(g.nodes.map((n) => [n.key, n.label, n.size, n.status, n.cls])).toEqual([
      ['g:r1.a', 'Team A', 6, 'reviewed', 'g:r1.a'],
      ['g:r1.b', 'Team B', 3, 'pending', 'g:r1.b'],
    ]);
    // links inside a community are its own count, not a loop
    expect(g.nodes[0].internal).toBe(5);
    expect(g.edges.map((e) => [e.source, e.target, e.count])).toEqual([['g:r1.a', 'g:r1.b', 1]]);
    expect(g.nodes[0].openable && g.nodes[0].children).toBe(2);
  });

  it('opens a community into the level below, under the colour of its root', () => {
    const base = communityLevel(reports(), (i) => i);
    const below = reports({
      within: {
        reports: { graph: 'urn:x-sparkles:graphrag:reports', level: 0 },
        group: 'r1.a',
        community: 'urn:c:A',
        level: 0,
        title: 'Team A',
        size: 6,
      },
      grouping: { kind: 'reports', predicate: null, level: 1 },
      groups: [
        {
          id: 'r1.a1',
          label: 'A one',
          size: 3,
          top: [],
          community: 'urn:c:A1',
          level: 1,
          children: 0,
          report: { iri: 'urn:r:A1', title: 'A one', status: 'stale' },
        },
        {
          id: 'r1.af',
          label: 'Members of no community below',
          size: 1,
          top: [],
          community: null,
          folded: true,
          level: 1,
          children: 0,
          report: null,
        },
      ],
      edges: [
        { source: 'g:r1.a1', target: 'g:r1.b', predicate: null, count: 1 },
        { source: 'g:r1.af', target: 'g:r1.a1', predicate: null, count: 1 },
      ],
    });
    const path = [{ key: 'g:r1.a', label: 'Team A', answer: below }];
    const g = visible(base, path);
    expect(g.nodes.map((n) => [n.key, n.cls, n.status])).toEqual([
      ['g:r1.b', 'g:r1.b', 'pending'],
      ['g:r1.a1', 'g:r1.a', 'stale'],
      ['g:r1.af', 'g:r1.a', 'pending'],
    ]);
    expect(g.edges.map((e) => [e.source, e.target]).sort()).toEqual([
      ['g:r1.a1', 'g:r1.b'],
      ['g:r1.af', 'g:r1.a1'],
    ]);
    expect(describe(g, path, String)).toBe('Team A opened into 2 communities.');
    expect(describe(base, [], String, 0)).toBe('GraphRAG communities at level 0: 2 communities.');
  });

  // review round 1: without WebGL every community was drawn in the one IRI colour, so the
  // status legend described colours the picture did not have
  it('keeps the colouring and the marks in the Cytoscape fallback', () => {
    const [a] = communityLevel(reports(), (i) => i).nodes;
    expect(fallbackNode(a, 'Team A (6)', '#1a7f37', false)).toEqual({
      id: 'g:r1.a',
      label: 'Team A (6)',
      kind: 'iri',
      title: 'r1.a',
      color: '#1a7f37',
    });
    expect(fallbackNode(a, 'Team A (6)', '#1a7f37', true).label).toBe('★ Team A (6)');
  });
});
