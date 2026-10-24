import { describe, expect, it } from 'vitest';
import { ApiError, type Term } from './api';
import {
  announcement,
  classifyFailure,
  defaultSettings,
  describePath,
  fmtCost,
  groupPaths,
  invalidReason,
  overlay,
  pathLimit,
  pathQuery,
  predicateWords,
  weightQuery,
} from './paths';
import { RDF_TYPE } from './rdf';

const EX = 'http://example.org/';
const iri = (n: string): Term => ({ type: 'uri', value: EX + n });
const local = (t: Term) => String(t.value).slice(EX.length);
const int = (n: number): Term => ({
  type: 'literal',
  value: String(n),
  datatype: 'http://www.w3.org/2001/XMLSchema#integer',
});
const row = (path: number, i: number, s: string, p: string, o: string, len: number) => ({
  path: int(path),
  i: int(i),
  s: iri(s),
  p: iri(p),
  o: iri(o),
  len: int(len),
});

describe('pathQuery', () => {
  const base = defaultSettings(iri('alice'), iri('lyon'));

  it('writes the defaults: one shortest path both ways without rdf:type and owl:sameAs', () => {
    const q = pathQuery(base);
    expect(q).toContain(
      'path:source <http://example.org/alice> ; path:target <http://example.org/lyon>',
    );
    expect(q).toContain('path:algorithm path:shortest ;');
    expect(q).toContain('path:maxLength 6 ;');
    expect(q).toContain('path:direction path:both ;');
    expect(q).toContain('path:maxDegree 1000 ;');
    expect(q).toContain(
      'path:excludePredicate <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> , <http://www.w3.org/2002/07/owl#sameAs> ;',
    );
    expect(q).toContain('path:maxVisited 1000000 ; path:limit 1 ;');
    expect(q).toContain('ORDER BY ?path ?i');
    expect(q).not.toContain('path:predicate');
    expect(q).not.toContain('path:k ');
  });

  it('writes k shortest with its lengths', () => {
    const q = pathQuery({ ...base, mode: 'kShortest', k: 5, minLength: 2, maxLength: 4 });
    expect(q).toContain('path:algorithm path:kShortest ;');
    expect(q).toContain('path:k 5 ; path:minLength 2 ; path:maxLength 4 ;');
    expect(q).toContain('path:limit 5 ;');
  });

  it('clamps k and the lengths', () => {
    const q = pathQuery({ ...base, mode: 'kShortest', k: 99, minLength: 9, maxLength: 30 });
    expect(q).toContain('path:k 20 ; path:minLength 8 ; path:maxLength 8 ;');
  });

  it('writes all shortest with its cap, and ignores the lower bound', () => {
    const q = pathQuery({ ...base, mode: 'allShortest', minLength: 3 });
    expect(q).toContain('path:limit 50 ;');
    expect(q).not.toContain('path:minLength');
  });

  it('follows only the listed predicates', () => {
    const q = pathQuery({ ...base, follow: 'only', predicates: [EX + 'knows', EX + 'worksFor'] });
    expect(q).toContain(
      'path:predicate <http://example.org/knows> , <http://example.org/worksFor> ;',
    );
    expect(q).not.toContain('excludePredicate');
  });

  it('writes no limit when the hub limit is off, and no exclusion for an empty list', () => {
    const q = pathQuery({ ...base, maxDegree: null, predicates: [] });
    expect(q).not.toContain('maxDegree');
    expect(q).not.toContain('excludePredicate');
  });

  it('falls back to explicit predicates on a server without Phase 1', () => {
    const q = pathQuery(base, false);
    expect(q).not.toContain('maxDegree');
    expect(q).not.toContain('excludePredicate');
    const only = pathQuery({ ...base, follow: 'only', predicates: [EX + 'knows'] }, false);
    expect(only).toContain('path:predicate <http://example.org/knows> ;');
  });

  it('ends a path at a literal', () => {
    const q = pathQuery({ ...base, target: { type: 'literal', value: 'Lyon' } });
    expect(q).toContain('path:target "Lyon" ;');
  });

  it('limits rows per mode', () => {
    expect(pathLimit({ mode: 'shortest', k: 3 })).toBe(1);
    expect(pathLimit({ mode: 'allShortest', k: 3 })).toBe(50);
    expect(pathLimit({ mode: 'kShortest', k: 7 })).toBe(7);
  });
});

describe('invalidReason', () => {
  it('refuses the same entity twice and a literal source', () => {
    expect(invalidReason(defaultSettings(iri('a'), iri('a')))).toBe(
      'Choose two different entities',
    );
    expect(invalidReason(defaultSettings({ type: 'literal', value: 'x' }, iri('a')))).toMatch(
      /literal/,
    );
    expect(invalidReason(defaultSettings(iri('a'), iri('b')))).toBeNull();
  });
});

// alice worksFor acme locatedIn lyon; alice knows bob worksFor beta locatedIn lyon
const ROWS = [
  row(0, 0, 'alice', 'worksFor', 'acme', 2),
  row(0, 1, 'acme', 'locatedIn', 'lyon', 2),
  row(1, 2, 'beta', 'locatedIn', 'lyon', 3),
  row(1, 0, 'alice', 'knows', 'bob', 3),
  row(1, 1, 'bob', 'worksFor', 'beta', 3),
];

describe('groupPaths', () => {
  it('groups rows into paths in order and orients edges from the source', () => {
    const ps = groupPaths(ROWS, iri('alice'));
    expect(ps.map((p) => [p.n, p.length])).toEqual([
      [1, 2],
      [2, 3],
    ]);
    expect(ps[1].nodes.map((t) => local(t))).toEqual(['alice', 'bob', 'beta', 'lyon']);
    expect(ps[1].edges.every((e) => e.forward)).toBe(true);
  });

  it('reads an edge against its triple', () => {
    // lyon to alice: both edges run against the stored triples
    const ps = groupPaths(
      [row(0, 0, 'acme', 'locatedIn', 'lyon', 2), row(0, 1, 'alice', 'worksFor', 'acme', 2)],
      iri('lyon'),
    );
    expect(ps[0].nodes.map((t) => local(t))).toEqual(['lyon', 'acme', 'alice']);
    expect(ps[0].edges.map((e) => e.forward)).toEqual([false, false]);
    expect(ps[0].edges[0].from).toBe('<http://example.org/lyon>');
  });

  it('keeps a path without edge rows', () => {
    const ps = groupPaths([{ path: int(0), len: int(0) }], iri('a'));
    expect(ps).toHaveLength(1);
    expect(ps[0].edges).toEqual([]);
  });
});

describe('overlay', () => {
  it('draws a shared edge once with every path number, in path then edge order', () => {
    const rows = [
      row(0, 0, 'a', 'p', 'b', 2),
      row(0, 1, 'b', 'p', 'd', 2),
      row(1, 0, 'a', 'p', 'b', 3),
      row(1, 1, 'b', 'p', 'c', 3),
      row(1, 2, 'c', 'p', 'd', 3),
    ];
    const o = overlay(groupPaths(rows, iri('a')));
    const edges = Object.fromEntries(o.edges.map((e) => [`${local(e.s)}${local(e.o)}`, e]));
    expect(edges.ab.paths).toEqual([1, 2]);
    expect(edges.bd.paths).toEqual([1]);
    expect(o.edges.map((e) => e.order)).toEqual([1, 2, 3, 4]);
    expect(Object.keys(edges)).toEqual(['ab', 'bd', 'bc', 'cd']);
    const nodes = Object.fromEntries(o.nodes.map((n) => [local(n.term), n]));
    expect(nodes.a.order).toBe(0);
    expect(nodes.c.order).toBe(3);
    expect(nodes.d.paths).toEqual([1, 2]);
    expect(o.nodes).toHaveLength(4);
  });
});

describe('words', () => {
  it('names predicates and paths', () => {
    expect(predicateWords(EX + 'worksFor')).toBe('works for');
    expect(predicateWords(EX + 'located_in')).toBe('located in');
    expect(predicateWords(RDF_TYPE)).toBe('type');
    const [p] = groupPaths(ROWS, iri('alice'));
    const cap = (t: Term) => {
      const s = local(t);
      return s[0].toUpperCase() + s.slice(1);
    };
    expect(describePath(p, cap)).toBe(
      'path 1, length 2, Alice works for Acme, Acme located in Lyon',
    );
    expect(announcement(groupPaths(ROWS, iri('alice')))).toBe(
      '2 paths found, the shortest of length 2',
    );
    expect(announcement([])).toBe('No path found');
  });
});

describe('classifyFailure', () => {
  it('tells the visit limit, the timeout and an old server apart', () => {
    expect(
      classifyFailure(new ApiError(400, 'path:search: a search visited more than 1000000 nodes'))
        .kind,
    ).toBe('visited');
    expect(classifyFailure(new ApiError(408, 'the query did not finish')).kind).toBe('timeout');
    expect(
      classifyFailure(new ApiError(400, 'path:search: unknown parameter path:excludePredicate'))
        .kind,
    ).toBe('unsupported');
    expect(classifyFailure(new Error('boom')).kind).toBe('other');
  });
});

describe('weights (Phase 3)', () => {
  const base = defaultSettings(iri('alice'), iri('lyon'));
  const dbl = (n: number): Term => ({
    type: 'literal',
    value: String(n),
    datatype: 'http://www.w3.org/2001/XMLSchema#double',
  });

  it('writes path:weight and binds the cost only with a weight', () => {
    const q = pathQuery({ ...base, weight: EX + 'km' });
    expect(q).toContain('path:weight <http://example.org/km> ;');
    expect(q).toContain('path:length ?len ; path:cost ?cost .');
    expect(q).toContain('SELECT ?path ?i ?s ?p ?o ?len ?cost WHERE');
    const plain = pathQuery(base);
    expect(plain).not.toContain('path:weight');
    expect(plain).not.toContain('?cost');
  });

  it('reads each path’s cost and tells it in words', () => {
    const rows = [
      { ...row(0, 0, 'alice', 'worksFor', 'acme', 2), cost: dbl(3.5) },
      { ...row(0, 1, 'acme', 'locatedIn', 'lyon', 2), cost: dbl(3.5) },
      { ...row(1, 0, 'alice', 'livesIn', 'lyon', 1), cost: dbl(7) },
    ];
    const paths = groupPaths(rows, iri('alice'));
    expect(paths.map((p) => [p.length, p.cost])).toEqual([
      [2, 3.5],
      [1, 7],
    ]);
    expect(describePath(paths[0], local)).toBe(
      'path 1, length 2, cost 3.5, alice works for acme, acme located in lyon',
    );
    expect(announcement(paths)).toBe('2 paths found, the cheapest of cost 3.5');
    // without weights no cost is read
    expect(groupPaths([row(0, 0, 'alice', 'livesIn', 'lyon', 1)], iri('alice'))[0].cost).toBe(
      undefined,
    );
  });

  it('formats costs with at most three decimals', () => {
    expect(fmtCost(3)).toBe('3');
    expect(fmtCost(0.1 + 0.2)).toBe('0.3');
    expect(fmtCost(1.23456)).toBe('1.235');
  });

  it('lists the numeric properties of reifiers, the most used first', () => {
    const q = weightQuery(5);
    expect(q).toContain('?r rdf:reifies ?t .');
    expect(q).toContain('FILTER(isNumeric(?v) && ?v >= 0)');
    expect(q).toContain('ORDER BY DESC(?n) ?w');
    expect(q).toContain('LIMIT 5');
  });
});
