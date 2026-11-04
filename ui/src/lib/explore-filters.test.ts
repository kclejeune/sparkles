import { describe, expect, it } from 'vitest';
import {
  applyFilters,
  classFacet,
  filtering,
  NO_CLASS,
  NO_FILTERS,
  predicateFacet,
  wantWebgl,
  type FEdge,
  type FNode,
} from './explore-filters';

const T = 'rdf:type';
const n = (id: string, kind = 'iri', via?: string): FNode => ({
  id,
  kind,
  ...(via ? { via } : {}),
});
const e = (s: string, p: string, t: string): FEdge => ({
  id: `${s}|${p}|${t}`,
  source: s,
  target: t,
  title: p,
  isType: p === T,
});

// a hub with three typed friends, a leaf, an untyped loner and a literal
const nodes = [
  n('hub'),
  n('a'),
  n('b'),
  n('c'),
  n('leaf'),
  n('loner'),
  n('Person'),
  n('"x"', 'literal'),
];
const edges = [
  e('hub', 'knows', 'a'),
  e('hub', 'knows', 'b'),
  e('hub', 'knows', 'c'),
  e('a', 'knows', 'b'),
  e('hub', 'owns', 'leaf'),
  e('a', T, 'Person'),
  e('b', T, 'Person'),
  e('c', T, 'Person'),
  e('hub', 'name', '"x"'),
];

describe('facets', () => {
  it('counts the edge types, most frequent first', () => {
    expect(predicateFacet(edges)).toEqual([
      { key: 'knows', count: 4 },
      { key: T, count: 3 },
      { key: 'name', count: 1 },
      { key: 'owns', count: 1 },
    ]);
  });
  it('counts the classes and puts the untyped last', () => {
    const f = classFacet(nodes, edges);
    expect(f[0]).toEqual({ key: 'Person', count: 3 });
    expect(f.at(-1)).toEqual({ key: NO_CLASS, count: 4 });
  });
  it('does not type group nodes or literals', () => {
    const f = classFacet([n('g', 'iri', 'knows'), n('l', 'literal')], []);
    expect(f).toEqual([]);
  });
});

describe('applyFilters', () => {
  const all = { ...NO_FILTERS };
  it('draws everything under the budget with no filters', () => {
    const d = applyFilters(nodes, edges, all);
    expect(d.nodes).toHaveLength(nodes.length);
    expect(d.edges).toHaveLength(edges.length);
    expect(d.hidden).toBe(0);
    expect(filtering(all)).toBe(false);
  });
  it('hides an edge type without loading less', () => {
    const d = applyFilters(nodes, edges, { ...all, predicates: new Set(['knows']) });
    expect(d.edges.some((x) => x.title === 'knows')).toBe(false);
    expect(d.edges).toHaveLength(5);
    expect(d.nodes).toHaveLength(nodes.length);
  });
  it('hides the members of a class together with their edges', () => {
    const d = applyFilters(nodes, edges, { ...all, classes: new Set(['Person']) });
    expect(d.nodes.some((x) => x.id === 'a')).toBe(false);
    expect(d.nodes.some((x) => x.id === 'hub')).toBe(true);
    expect(d.edges.some((x) => x.source === 'a' || x.target === 'a')).toBe(false);
  });
  it('hides untyped nodes through the no-class entry', () => {
    const d = applyFilters(nodes, edges, { ...all, classes: new Set([NO_CLASS]) });
    expect(d.nodes.map((x) => x.id).sort()).toEqual(['"x"', 'a', 'b', 'c']);
  });
  it('hides nodes under a minimum degree and the edges to them', () => {
    const d = applyFilters(nodes, edges, { ...all, minDegree: 3 });
    expect(d.nodes.map((x) => x.id).sort()).toEqual(['Person', 'a', 'b', 'hub']);
    expect(d.edges.every((x) => x.target !== 'leaf' && x.target !== 'c')).toBe(true);
  });
  it('hides leaves, which are nodes of one drawn edge', () => {
    const d = applyFilters(nodes, edges, { ...all, hideLeaves: true });
    expect(d.nodes.some((x) => x.id === 'leaf')).toBe(false);
    expect(d.nodes.some((x) => x.id === 'hub')).toBe(true);
    expect(d.nodes.some((x) => x.id === 'loner')).toBe(false);
  });
  it('counts degree after the edge filter', () => {
    // without type edges, c has one edge and is a leaf
    const d = applyFilters(nodes, edges, { ...all, hideLeaves: true, predicates: new Set([T]) });
    expect(d.nodes.some((x) => x.id === 'c')).toBe(false);
    expect(d.nodes.some((x) => x.id === 'a')).toBe(true);
  });
  it('keeps the selection whatever the filters say', () => {
    const d = applyFilters(nodes, edges, { ...all, minDegree: 9 }, new Set(['loner']));
    expect(d.nodes.map((x) => x.id)).toEqual(['loner']);
  });
  it('hides the nodes of a hidden group', () => {
    const d = applyFilters(nodes, edges, { ...all, hiddenIds: new Set(['a', 'b']) });
    expect(d.nodes.some((x) => x.id === 'a' || x.id === 'b')).toBe(false);
    expect(d.hidden).toBe(2);
  });
  it('hides a group node with its predicate', () => {
    const g = [...nodes, n('g', 'iri', 'knows')];
    const ge = [...edges, e('hub', 'knows', 'g')];
    const d = applyFilters(g, ge, { ...all, predicates: new Set(['knows']) });
    expect(d.nodes.some((x) => x.id === 'g')).toBe(false);
  });
  it('draws the most connected nodes within the budget', () => {
    const d = applyFilters(nodes, edges, { ...all, budget: 2 });
    expect(d.nodes.map((x) => x.id)).toEqual(['hub', 'Person']);
    expect(d.overBudget).toBe(nodes.length - 2);
    expect(d.hidden).toBe(nodes.length - 2);
    expect(d.edges).toHaveLength(0);
  });
  it('spends the budget on the kept nodes first', () => {
    const d = applyFilters(nodes, edges, { ...all, budget: 2 }, new Set(['loner']));
    expect(d.nodes.map((x) => x.id)).toContain('loner');
    expect(d.nodes).toHaveLength(2);
  });
});

describe('wantWebgl', () => {
  it('switches past the threshold and back with some slack', () => {
    expect(wantWebgl(1000, false)).toBe(false);
    expect(wantWebgl(1001, false)).toBe(true);
    expect(wantWebgl(900, true)).toBe(true);
    expect(wantWebgl(700, true)).toBe(false);
  });
  it('follows a lowered threshold', () => {
    expect(wantWebgl(30, false, 20)).toBe(true);
    expect(wantWebgl(15, true, 20)).toBe(false);
  });
});
