import { describe, expect, it } from 'vitest';
import { SELECTION_CAP, SharedSelection } from './selection.svelte';

describe('the shared selection', () => {
  it('starts empty and replaces what it holds', () => {
    const s = new SharedSelection();
    expect(s.size).toBe(0);
    s.set(['<a>', '<b>'], 'the map');
    expect(s.size).toBe(2);
    expect(s.has('<a>')).toBe(true);
    expect(s.source).toBe('the map');
    s.set(['<c>'], 'the graph');
    expect([...s.ids]).toEqual(['<c>']);
    expect(s.source).toBe('the graph');
  });

  it('adds, removes and toggles', () => {
    const s = new SharedSelection();
    s.set(['<a>'], 'the map');
    s.add(['<b>', '<a>'], 'the graph');
    expect([...s.ids]).toEqual(['<a>', '<b>']);
    // the source stays that of the selection that was started
    expect(s.source).toBe('the map');
    s.remove(['<a>']);
    expect([...s.ids]).toEqual(['<b>']);
    s.toggle(['<b>', '<c>'], 'the map');
    expect([...s.ids].sort()).toEqual(['<b>', '<c>']);
    s.toggle(['<b>', '<c>'], 'the map');
    expect(s.size).toBe(0);
    expect(s.source).toBe('');
  });

  it('clears, and says so', () => {
    const s = new SharedSelection();
    s.set(['<a>'], 'the map');
    expect(s.announcement).toBe('1 selected by the map');
    s.clear();
    expect(s.size).toBe(0);
    expect(s.source).toBe('');
    expect(s.announcement).toBe('Selection cleared');
  });

  it('replaces the set on every change, so that readers see it', () => {
    const s = new SharedSelection();
    const before = s.ids;
    s.add(['<a>'], 'x');
    expect(s.ids).not.toBe(before);
    expect(before.size).toBe(0);
  });

  it('keeps the first 20,000 and counts the rest', () => {
    const s = new SharedSelection();
    const ids = Array.from({ length: SELECTION_CAP + 7 }, (_, i) => `<e${i}>`);
    s.set(ids, 'the map');
    expect(s.size).toBe(SELECTION_CAP);
    expect(s.dropped).toBe(7);
    expect(s.has('<e0>')).toBe(true);
    expect(s.has(`<e${SELECTION_CAP}>`)).toBe(false);
    expect(s.announcement).toContain('7 more left out');
    // adding to a full selection drops too
    s.add(['<more>'], 'the map');
    expect(s.dropped).toBe(8);
    s.clear();
    expect(s.dropped).toBe(0);
  });

  it('ignores repeated entities', () => {
    const s = new SharedSelection();
    s.set(['<a>', '<a>', '<b>'], 'x');
    expect(s.size).toBe(2);
  });
});
