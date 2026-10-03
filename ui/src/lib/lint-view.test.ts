import { EditorState } from '@codemirror/state';
import { describe, expect, it } from 'vitest';
import type { LintDiagnostic } from './api';
import { lintCounts, lintField, lintMarks, lintSummary, setLintDiagnostics } from './lint-view';

const d = (over: Partial<LintDiagnostic>): LintDiagnostic => ({
  rule: 'unused-prefix',
  severity: 'warning',
  message: 'the prefix ex: is declared but never used',
  line: 1,
  column: 1,
  endLine: 1,
  endColumn: 5,
  from: 0,
  to: 4,
  ...over,
});

describe('lint marks', () => {
  it('marks each finding by severity, with its message as the title', () => {
    expect(
      lintMarks(
        [d({ from: 7, to: 9, severity: 'error', rule: 'syntax', message: 'm' }), d({})],
        20,
      ),
    ).toEqual([
      {
        from: 0,
        to: 4,
        className: 'cm-lint cm-lint-warning',
        title: 'the prefix ex: is declared but never used [unused-prefix]',
      },
      { from: 7, to: 9, className: 'cm-lint cm-lint-error', title: 'm [syntax]' },
    ]);
  });

  it('keeps empty ranges visible and clamps to the document', () => {
    const marks = lintMarks(
      [d({ from: 3, to: 3 }), d({ from: 10, to: 10 }), d({ from: 8, to: 99 })],
      10,
    );
    expect(marks.map((m) => [m.from, m.to])).toEqual([
      [3, 4],
      [8, 10],
      [9, 10],
    ]);
    expect(lintMarks([d({ from: 0, to: 0 })], 0)).toEqual([]);
  });

  it('follows edits until the next findings', () => {
    let state = EditorState.create({ doc: 'PREFIX ex: <x>\nASK {}', extensions: [lintField] });
    state = state.update({ effects: setLintDiagnostics.of([d({ from: 0, to: 14 })]) }).state;
    state = state.update({ changes: { from: 0, insert: '# a\n' } }).state;
    const ranges: [number, number][] = [];
    state.field(lintField).between(0, state.doc.length, (from, to) => {
      ranges.push([from, to]);
    });
    expect(ranges).toEqual([[4, 18]]);
    state = state.update({ effects: setLintDiagnostics.of([]) }).state;
    expect(state.field(lintField).size).toBe(0);
  });

  it('summarizes the findings', () => {
    const all = [
      d({ severity: 'error' }),
      d({ severity: 'warning' }),
      d({ severity: 'warning' }),
      d({ severity: 'hint' }),
    ];
    expect(lintCounts(all)).toEqual({ error: 1, warning: 2, info: 0, hint: 1 });
    expect(lintSummary(all)).toBe('1 error, 2 warnings');
    expect(lintSummary([d({ severity: 'hint' }), d({ severity: 'info' })])).toBe('1 note, 1 hint');
    expect(lintSummary([])).toBe('');
  });
});
