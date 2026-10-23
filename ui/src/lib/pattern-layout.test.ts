import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { kept, LAYOUT_KEY, LayoutStore, MAX_LAYOUTS, textKey } from './pattern-layout';

/** A localStorage stand-in. */
function storage() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
    m,
  };
}

let ls: ReturnType<typeof storage>;
beforeEach(() => {
  ls = storage();
  vi.stubGlobal('localStorage', ls);
});
afterEach(() => vi.unstubAllGlobals());

describe('the layout map', () => {
  it('keys a query by its text without whitespace', () => {
    expect(textKey('SELECT * { ?s ?p ?o }')).toBe(textKey('SELECT *\n{\n  ?s ?p ?o\n}\n'));
    expect(textKey('SELECT * { ?s ?p ?o }')).not.toBe(textKey('SELECT * { ?s ?p ?x }'));
  });

  it('keeps a tab’s layout in memory and restores a stored one for the same text', () => {
    const a = new LayoutStore();
    const l = a.get('tab1', 'ASK { ?a ?b ?c }');
    expect(l).toEqual({});
    a.set('tab1', 'ASK { ?a ?b ?c }', { 'v:a': { x: 1, y: 2 } });
    // the tab keeps its map while the text changes
    expect(a.get('tab1', 'ASK { ?a ?b ?d }')).toEqual({ 'v:a': { x: 1, y: 2 } });
    // another session opening the same query gets the stored map
    const b = new LayoutStore();
    expect(b.get('other', 'ASK {\n  ?a ?b ?c\n}')).toEqual({ 'v:a': { x: 1, y: 2 } });
    expect(b.get('fresh', 'ASK { ?x ?y ?z }')).toEqual({});
  });

  it('moves a renamed variable’s position', () => {
    const a = new LayoutStore();
    a.set('t', 'q', { 'v:org': { x: 5, y: 6 } });
    a.rename('t', 'v:org', 'v:company');
    expect(a.get('t', 'q')).toEqual({ 'v:company': { x: 5, y: 6 } });
  });

  it('stores at most 200 maps, dropping the oldest', () => {
    let t = 0;
    const a = new LayoutStore(() => ++t);
    for (let i = 0; i < MAX_LAYOUTS + 5; i++)
      a.set('t', `ASK { ?a ?b ${i} }`, { k: { x: i, y: 0 } });
    const stored = JSON.parse(ls.getItem(LAYOUT_KEY)!).entries;
    expect(stored).toHaveLength(MAX_LAYOUTS);
    expect(a.stored('ASK { ?a ?b 0 }')).toBeNull();
    expect(a.stored('ASK { ?a ?b 5 }')).toEqual({ k: { x: 5, y: 0 } });
    // writing an old one again makes it the newest
    a.set('t', 'ASK { ?a ?b 5 }', { k: { x: 50, y: 0 } });
    a.set('t', 'ASK { ?a ?b new }', { k: { x: 1, y: 0 } });
    expect(a.stored('ASK { ?a ?b 5 }')).toEqual({ k: { x: 50, y: 0 } });
    expect(a.stored('ASK { ?a ?b 6 }')).toBeNull();
  });

  it('works without storage', () => {
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    });
    const a = new LayoutStore();
    a.set('t', 'q', { k: { x: 1, y: 1 } });
    expect(a.get('t', 'q')).toEqual({ k: { x: 1, y: 1 } });
    expect(a.stored('q')).toBeNull();
    ls.setItem(LAYOUT_KEY, '{"entries": 3}');
  });

  it('keeps only the keys still drawn', () => {
    expect(kept({ a: { x: 1, y: 1 }, b: { x: 2, y: 2 } }, ['b', 'c'])).toEqual({
      b: { x: 2, y: 2 },
    });
  });

  it('ignores a corrupt store', () => {
    ls.setItem(LAYOUT_KEY, '{"entries": 3}');
    expect(new LayoutStore().stored('q')).toBeNull();
  });
});
