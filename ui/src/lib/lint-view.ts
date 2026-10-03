// Lint findings in a CodeMirror editor (spec X03): a mark per finding, styled by severity,
// with the message as its title. The marks follow the text while it is edited and are
// replaced when the next findings arrive.

import { StateEffect, StateField, type Extension } from '@codemirror/state';
import { Decoration, EditorView, type DecorationSet } from '@codemirror/view';
import type { LintDiagnostic, LintSeverity } from './api';

/** Replace the editor's findings. */
export const setLintDiagnostics = StateEffect.define<LintDiagnostic[]>();

/** One mark: where it goes, its class and its title. */
export type LintMark = { from: number; to: number; className: string; title: string };

/**
 * The marks of `diagnostics` in a document of `length` UTF-16 units. An empty range gets
 * the character after it, or before it at the end of the document, so it stays visible.
 */
export function lintMarks(diagnostics: LintDiagnostic[], length: number): LintMark[] {
  const marks: LintMark[] = [];
  for (const d of diagnostics) {
    let from = Math.max(0, Math.min(d.from, length));
    let to = Math.max(from, Math.min(d.to, length));
    if (from === to) {
      if (to < length) to += 1;
      else if (from > 0) from -= 1;
      else continue;
    }
    marks.push({
      from,
      to,
      className: `cm-lint cm-lint-${d.severity}`,
      title: `${d.message} [${d.rule}]`,
    });
  }
  return marks.sort((a, b) => a.from - b.from || a.to - b.to);
}

export const lintField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (!e.is(setLintDiagnostics)) continue;
      const marks = lintMarks(e.value, tr.state.doc.length);
      deco = Decoration.set(
        marks.map((m) =>
          Decoration.mark({ class: m.className, attributes: { title: m.title } }).range(
            m.from,
            m.to,
          ),
        ),
        true,
      );
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/** The theme of the marks: a wavy underline in the severity's colour. */
export const lintTheme = EditorView.theme({
  '.cm-lint': { textDecorationLine: 'underline', textUnderlineOffset: '3px' },
  '.cm-lint-error': { textDecorationStyle: 'wavy', textDecorationColor: 'var(--danger)' },
  '.cm-lint-warning': { textDecorationStyle: 'wavy', textDecorationColor: 'var(--warn)' },
  '.cm-lint-info': { textDecorationStyle: 'dotted', textDecorationColor: 'var(--iri)' },
  '.cm-lint-hint': { textDecorationStyle: 'dotted', textDecorationColor: 'var(--text-3)' },
});

/** The editor extensions for lint findings. */
export function lintView(): Extension {
  return [lintField, lintTheme];
}

/** How many findings of each severity. */
export function lintCounts(diagnostics: LintDiagnostic[]): Record<LintSeverity, number> {
  const c: Record<LintSeverity, number> = { error: 0, warning: 0, info: 0, hint: 0 };
  for (const d of diagnostics) c[d.severity] += 1;
  return c;
}

/** A short summary of the findings for a status line ("1 error, 2 warnings"); hints and
 * information are left out when there is anything worse. */
export function lintSummary(diagnostics: LintDiagnostic[]): string {
  const c = lintCounts(diagnostics);
  const part = (n: number, one: string, many: string) => (n ? `${n} ${n === 1 ? one : many}` : '');
  const main = [part(c.error, 'error', 'errors'), part(c.warning, 'warning', 'warnings')].filter(
    Boolean,
  );
  if (main.length) return main.join(', ');
  return [part(c.info, 'note', 'notes'), part(c.hint, 'hint', 'hints')].filter(Boolean).join(', ');
}
