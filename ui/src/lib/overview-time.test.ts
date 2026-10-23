import { describe, expect, it } from 'vitest';
import type { ClassProfile, DiffGroup, GroupedDiff } from './api';
import { applyOverviewChanges, classLevel, overviewChanges } from './overview';

const EX = 'http://example.org/';
const T = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';

const profile = (
  cls: string,
  instances: number,
  links: [string, string, number][] = [],
): ClassProfile =>
  ({
    class: `${EX}${cls}`,
    builtin: false,
    instances,
    incoming: [],
    properties: links.map(([p, oc, triples]) => ({
      predicate: `${EX}${p}`,
      objectClasses: [{ class: `${EX}${oc}`, triples }],
    })),
  }) as unknown as ClassProfile;

const grouped = (groups: DiffGroup[]): GroupedDiff => ({
  dataset: 'ds',
  datasetId: 'x',
  from: { selector: '3', commit: { seq: 3 } as never },
  to: { selector: '4', commit: { seq: 4 } as never },
  added: 0,
  removed: 0,
  method: 'log',
  group: 'class',
  groupCount: groups.length,
  truncated: false,
  groups,
});

describe('the overview through time', () => {
  const base = classLevel([profile('Person', 3, [['worksFor', 'Org', 2]]), profile('Org', 1)]);
  const step = grouped([
    {
      subjectClass: `${EX}Person`,
      predicate: T,
      object: { class: `${EX}Person` },
      added: 600,
      removed: 0,
    },
    {
      subjectClass: `${EX}Person`,
      predicate: `${EX}worksFor`,
      object: { class: `${EX}Org` },
      added: 600,
      removed: 0,
    },
    {
      subjectClass: `${EX}Person`,
      predicate: `${EX}knows`,
      object: { class: `${EX}Robot` },
      added: 1,
      removed: 0,
    },
    {
      subjectClass: `${EX}Robot`,
      predicate: T,
      object: { class: `${EX}Robot` },
      added: 1,
      removed: 0,
    },
    {
      subjectClass: `${EX}Person`,
      predicate: `${EX}name`,
      object: { literal: 'x' },
      added: 9,
      removed: 0,
    },
  ]);

  it('reads class and edge changes from a grouped diff (A14)', () => {
    const c = overviewChanges(step);
    expect(c.classes.get(`${EX}Person`)).toEqual({ added: 600, removed: 0 });
    expect(c.edges.get(`c:${EX}Person\u0001c:${EX}Org`)?.added).toBe(600);
    // literals have no class edge
    expect([...c.edges.keys()]).toHaveLength(2);
  });

  it('moves sizes and edge counts, adds new classes and keeps keys', () => {
    const next = applyOverviewChanges(base, overviewChanges(step));
    const size = (k: string) => next.nodes.find((n) => n.key === `c:${EX}${k}`)?.size;
    expect(size('Person')).toBe(603);
    expect(size('Robot')).toBe(1);
    expect(next.edges.find((e) => e.key === `c:${EX}Person\u0001c:${EX}Org`)?.count).toBe(602);
    expect(next.edges.find((e) => e.key === `c:${EX}Person\u0001c:${EX}Robot`)?.count).toBe(1);
    // and back again: what falls to nothing goes
    const back = grouped(step.groups.map((g) => ({ ...g, added: g.removed, removed: g.added })));
    const again = applyOverviewChanges(next, overviewChanges(back));
    expect(again.nodes.map((n) => n.key).sort()).toEqual(base.nodes.map((n) => n.key).sort());
    expect(again.edges.map((e) => [e.key, e.count])).toEqual(
      base.edges.map((e) => [e.key, e.count]),
    );
  });
});
