import { describe, expect, it } from 'vitest';
import type { FormatRequest, FormatResult } from './api';
import { formatEditor, minimalChange, placeCursor, type Change } from './fmt-edit';

const apply = (text: string, c: Change | null) =>
  c ? text.slice(0, c.from) + c.insert + text.slice(c.to) : text;

describe('minimalChange', () => {
  it('is null for equal texts', () => {
    expect(minimalChange('ASK {}', 'ASK {}')).toBeNull();
  });

  it('trims the common prefix and suffix', () => {
    const before = 'SELECT * WHERE {\n?s ?p ?o\n}\n';
    const after = 'SELECT * WHERE {\n  ?s ?p ?o .\n}\n';
    const c = minimalChange(before, after);
    expect(c).toEqual({ from: 17, to: 25, insert: '  ?s ?p ?o .' });
    expect(apply(before, c)).toBe(after);
  });

  it('handles pure insertions, deletions and whole replacements', () => {
    for (const [a, b] of [
      ['ab', 'aXb'],
      ['aXb', 'ab'],
      ['', 'x'],
      ['x', ''],
      ['abc', 'xyz'],
      ['aaa', 'aaaa'],
      ['aaaa', 'aa'],
    ]) {
      const c = minimalChange(a, b)!;
      expect(apply(a, c), `${a} -> ${b}`).toBe(b);
      expect(c.to - c.from + c.insert.length).toBeLessThanOrEqual(a.length + b.length);
    }
    // repeated characters: the change stays minimal
    expect(minimalChange('aaa', 'aaaa')).toEqual({ from: 3, to: 3, insert: 'a' });
  });

  it('never splits a surrogate pair', () => {
    // 😀 is 😀, 😁 is 😁: they share the high half
    for (const [a, b] of [
      ['x😀y', 'x😁y'],
      ['😀', '😁'],
      ['a😀', 'a😁'],
      ['😀b', '😁b'],
      ['("😀" AS ?x)', '("😁" AS ?x)'],
    ]) {
      const c = minimalChange(a, b)!;
      expect(apply(a, c)).toBe(b);
      const halves = [c.insert, a.slice(c.from, c.to)];
      for (const h of halves) {
        // every part of the change is well formed on its own
        expect(h).toBe(String.fromCodePoint(...Array.from(h).map((ch) => ch.codePointAt(0)!)));
        expect(
          /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(h),
        ).toBe(false);
      }
    }
  });
});

describe('placeCursor', () => {
  it('clamps and avoids the middle of a pair', () => {
    expect(placeCursor('abc', null)).toBeNull();
    expect(placeCursor('abc', undefined)).toBeNull();
    expect(placeCursor('abc', 2)).toBe(2);
    expect(placeCursor('abc', 9)).toBe(3);
    expect(placeCursor('abc', -1)).toBe(0);
    expect(placeCursor('a😀b', 2)).toBe(1);
    expect(placeCursor('a😀b', 3)).toBe(3);
  });
});

/** An editor double: a text, a cursor, and the replacements it received. */
function target(text: string, cursor = 0) {
  const t = {
    text,
    cursor,
    replaced: [] as { text: string; cursor: number | null }[],
    snapshot: () => ({ text: t.text, cursorOffset: t.cursor }),
    replaceFormatted(text: string, cursor: number | null) {
      t.replaced.push({ text, cursor });
      t.text = text;
      if (cursor != null) t.cursor = cursor;
    },
  };
  return t;
}

const result = (text: string, cursorOffset: number | null = null): FormatResult => ({
  text,
  changed: true,
  language: 'sparql',
  cursorOffset,
  warnings: [],
});

describe('formatEditor', () => {
  it('sends the text and cursor, and applies the result with the mapped cursor', async () => {
    const ed = target('ask{}', 3);
    let sent: FormatRequest | undefined;
    const outcome = await formatEditor(ed, async (req) => {
      sent = req;
      return result('ASK {}\n', 4);
    });
    expect(sent).toEqual({ text: 'ask{}', language: 'sparql', cursorOffset: 3 });
    expect(outcome).toBe('formatted');
    expect(ed.replaced).toEqual([{ text: 'ASK {}\n', cursor: 4 }]);
  });

  it('does nothing when the text is formatted already', async () => {
    const ed = target('ASK {}\n');
    expect(await formatEditor(ed, async () => result('ASK {}\n', 0))).toBe('unchanged');
    expect(ed.replaced).toEqual([]);
  });

  it('drops a result when the text changed while the request was out', async () => {
    const ed = target('ask{}');
    let release!: (r: FormatResult) => void;
    const pending = formatEditor(ed, () => new Promise((r) => (release = r)));
    ed.text = 'ask{} # edited';
    release(result('ASK {}\n'));
    expect(await pending).toBe('stale');
    expect(ed.replaced).toEqual([]);
    expect(ed.text).toBe('ask{} # edited');
  });

  it('applies the result when the text was changed and changed back', async () => {
    const ed = target('ask{}');
    let release!: (r: FormatResult) => void;
    const pending = formatEditor(ed, () => new Promise((r) => (release = r)));
    ed.text = 'ask{} x';
    ed.text = 'ask{}';
    release(result('ASK {}\n'));
    expect(await pending).toBe('formatted');
  });

  it('clamps a cursor beyond the formatted text', async () => {
    const ed = target('ask{}', 5);
    await formatEditor(ed, async () => result('ASK {}', 99));
    expect(ed.replaced[0].cursor).toBe(6);
  });

  it('lets errors through without touching the editor', async () => {
    const ed = target('select * {');
    const err = Object.assign(new Error('syntax'), { status: 400, line: 1, column: 11 });
    await expect(
      formatEditor(ed, async () => {
        throw err;
      }),
    ).rejects.toBe(err);
    expect(ed.replaced).toEqual([]);
  });
});
