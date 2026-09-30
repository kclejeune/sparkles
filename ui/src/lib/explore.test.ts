import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import { hierarchyRoots, pickLabel, reduceSupers, type ClassInfo } from './explore';

/** Class map from `sub → supers` edges, with `subs` filled in as loadSchema does. */
function classesOf(edges: Record<string, string[]>, labels: Record<string, string> = {}) {
  const classes = new Map<string, ClassInfo>();
  const get = (iri: string) => {
    let c = classes.get(iri);
    if (!c) classes.set(iri, (c = { iri, supers: [], subs: [], instances: 0, declared: true }));
    return c;
  };
  for (const [c, supers] of Object.entries(edges)) {
    get(c).supers = [...supers];
    for (const s of supers) get(s).subs.push(c);
  }
  for (const [c, l] of Object.entries(labels)) get(c).label = l;
  return classes;
}

describe('reduceSupers', () => {
  it('drops supers implied by other supers', () => {
    const r = reduceSupers(
      new Map([
        ['A', ['B', 'C']],
        ['B', ['C']],
        ['C', []],
      ]),
    );
    expect(r.get('A')).toEqual(['B']);
    expect(r.get('B')).toEqual(['C']);
  });

  it('reduces a fully closed hierarchy to a chain', () => {
    const r = reduceSupers(
      new Map([
        ['D', ['C', 'B', 'A']],
        ['C', ['B', 'A']],
        ['B', ['A']],
      ]),
    );
    expect(r.get('D')).toEqual(['C']);
    expect(r.get('C')).toEqual(['B']);
  });

  it('keeps both members of an equivalence cycle', () => {
    const r = reduceSupers(
      new Map([
        ['A', ['B', 'C']],
        ['B', ['A', 'C']],
        ['C', []],
      ]),
    );
    expect(r.get('A')).toEqual(['B']);
    expect(r.get('B')).toEqual(['A']);
  });

  it('never empties a non-empty super list', () => {
    const r = reduceSupers(new Map([['A', ['A']]]));
    expect(r.get('A')).toEqual(['A']);
  });
});

describe('hierarchyRoots', () => {
  it('ranks roots by subclass count, then by label', () => {
    const classes = classesOf(
      { B1: ['B'], B2: ['B'], A1: ['A'], Z: [], Y: [] },
      { Z: 'alpha', Y: 'beta' },
    );
    expect(hierarchyRoots(classes)).toEqual(['B', 'A', 'Z', 'Y']);
  });

  // A ⊑ B ⊑ A with nothing above: both have supers, so neither is an ordinary root.
  it('promotes one member of a rootless cycle', () => {
    const roots = hierarchyRoots(classesOf({ A: ['B'], B: ['A'], C: ['A'] }));
    expect(roots).toEqual(['A']);
  });

  it('promotes a class whose only super is itself', () => {
    expect(hierarchyRoots(classesOf({ D: ['D'], E: [] }))).toEqual(['E', 'D']);
  });

  it('does not add a cycle that hangs below an ordinary root', () => {
    expect(hierarchyRoots(classesOf({ A: ['B', 'T'], B: ['A'], T: [] }))).toEqual(['T']);
  });

  it('reaches every class', () => {
    const classes = classesOf({ A: ['B'], B: ['C'], C: ['A'], X: ['Y'], Y: ['X'], Q: [] });
    const roots = hierarchyRoots(classes);
    const seen = new Set<string>();
    const walk = (iri: string) => {
      if (seen.has(iri)) return;
      seen.add(iri);
      classes.get(iri)!.subs.forEach(walk);
    };
    roots.forEach(walk);
    expect(seen.size).toBe(classes.size);
    expect(roots.length).toBe(3);
  });
});

describe('pickLabel', () => {
  const lit = (value: string, lang?: string): Term =>
    lang ? { type: 'literal', value, 'xml:lang': lang } : { type: 'literal', value };

  it('prefers English, then untagged, then anything', () => {
    expect(pickLabel([lit('Hund', 'de'), lit('dog'), lit('Dog', 'en-GB')])).toBe('Dog');
    expect(pickLabel([lit('Hund', 'de'), lit('dog')])).toBe('dog');
    expect(pickLabel([lit('Hund', 'de')])).toBe('Hund');
  });

  it('ignores non-literals and unbound values', () => {
    expect(pickLabel([undefined, { type: 'uri', value: 'http://ex.org/a' }])).toBeUndefined();
  });
});
