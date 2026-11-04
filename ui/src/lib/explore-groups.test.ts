import { describe, expect, it } from 'vitest';
import {
  classQuery,
  countQuery,
  groupCaption,
  groupId,
  isGroupId,
  memberQuery,
  pageOf,
  planFanout,
  type Fan,
  type GroupSpec,
} from './explore-groups';

const fan = (predicate: string, count: number, incoming = false): Fan => ({
  predicate,
  count,
  incoming,
});
const spec: GroupSpec = {
  owner: '<http://e/o>',
  ownerIri: 'http://e/o',
  predicate: 'http://e/knows',
  incoming: false,
  total: 500,
};

describe('planFanout', () => {
  it('draws small predicates and groups the large ones', () => {
    const p = planFanout([fan('a', 3), fan('b', 400), fan('c', 26), fan('d', 25, true)]);
    expect(p.inline.map((f) => f.predicate).sort()).toEqual(['a', 'd']);
    expect(p.groups.map((f) => f.predicate)).toEqual(['b', 'c']);
  });
  it('groups the largest small predicates once the inline budget is spent', () => {
    const fans = [
      fan('a', 20),
      fan('b', 20),
      fan('c', 20),
      fan('d', 20),
      fan('e', 20),
      fan('f', 20),
    ];
    const p = planFanout(fans, 25, 100);
    expect(p.inline).toHaveLength(5);
    expect(p.groups).toHaveLength(1);
  });
  it('keeps the smallest predicates inline first', () => {
    const p = planFanout([fan('big', 24), fan('tiny', 1), fan('mid', 20)], 25, 30);
    expect(p.inline.map((f) => f.predicate)).toEqual(['tiny', 'mid']);
    expect(p.groups.map((f) => f.predicate)).toEqual(['big']);
  });
  it('treats a direction as its own fan', () => {
    const p = planFanout([fan('p', 20), fan('p', 20, true)]);
    expect(p.inline).toHaveLength(2);
  });
});

describe('group ids and captions', () => {
  it('names a group by its owner, predicate and direction', () => {
    expect(groupId('<o>', 'p', false)).not.toBe(groupId('<o>', 'p', true));
    expect(isGroupId(groupId('<o>', 'p', false))).toBe(true);
    expect(isGroupId('<http://x>')).toBe(false);
  });
  it('writes the count with separators', () => {
    expect(groupCaption(1204, 'ex:knows')).toBe('1,204 via ex:knows');
  });
});

describe('queries', () => {
  it('reads outgoing members in a stable order', () => {
    const q = memberQuery(spec, { filter: '', cls: null, offset: 0 });
    expect(q).toContain('<http://e/o> <http://e/knows> ?m');
    expect(q).toContain('ORDER BY ?m');
    expect(q).toContain('LIMIT 100');
    expect(q).not.toContain('OFFSET');
    expect(q).not.toContain('FILTER');
  });
  it('reads incoming members, a later page, a class and a filter', () => {
    const q = memberQuery(
      { ...spec, incoming: true },
      { filter: ' Ada "L" ', cls: 'http://e/Person', offset: 200 },
    );
    expect(q).toContain('?m <http://e/knows> <http://e/o>');
    expect(q).toContain('?m rdf:type <http://e/Person>');
    expect(q).toContain('OFFSET 200');
    expect(q).toContain('CONTAINS(LCASE(STR(COALESCE(?l, ?m))), "ada \\"l\\"")');
  });
  it('counts distinct members under the same filter', () => {
    const q = countQuery(spec, { filter: 'x', cls: 'http://e/C' });
    expect(q).toContain('COUNT(DISTINCT ?m)');
    expect(q).toContain('rdf:type <http://e/C>');
    expect(countQuery(spec, { filter: '', cls: null })).not.toContain('OPTIONAL');
  });
  it('splits members by class', () => {
    const q = classQuery(spec, '');
    expect(q).toContain('?m rdf:type ?c');
    expect(q).toContain('GROUP BY ?c');
    expect(classQuery(spec, 'ab')).toContain('"ab"');
  });
});

describe('pageOf', () => {
  it('pages 250 members by 100', () => {
    expect(pageOf(250, 0)).toEqual({ from: 1, to: 100, page: 1, pages: 3, prev: null, next: 100 });
    expect(pageOf(250, 200)).toEqual({
      from: 201,
      to: 250,
      page: 3,
      pages: 3,
      prev: 100,
      next: null,
    });
  });
  it('shows an empty group as one empty page', () => {
    expect(pageOf(0, 0)).toEqual({ from: 0, to: 0, page: 1, pages: 1, prev: null, next: null });
  });
  it('clamps an offset past the end', () => {
    expect(pageOf(120, 1000).page).toBe(2);
  });
});
