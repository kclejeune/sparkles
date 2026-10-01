// Formatting a CodeMirror editor other than the query editor (the SHACL shapes graph):
// the editor as a `FormatTarget`, and what to tell the user when formatting fails.

import { isolateHistory } from '@codemirror/commands';
import type { EditorState, TransactionSpec } from '@codemirror/state';
import { ApiError } from './api';
import { minimalChange, type FormatTarget } from './fmt-edit';

/** The part of an `EditorView` the formatting flow needs. */
export type ViewLike = {
  readonly state: EditorState;
  dispatch(tr: TransactionSpec): void;
};

/**
 * The editor as a format target: its text and cursor (UTF-16 code units), and the
 * formatted text applied as one change (common prefix and suffix trimmed) that undo
 * reverts on its own, with the cursor where the server mapped it (or mapped through the
 * change without one).
 */
export function viewTarget(view: () => ViewLike | undefined): FormatTarget {
  return {
    snapshot() {
      const v = view();
      return {
        text: v?.state.doc.toString() ?? '',
        cursorOffset: v?.state.selection.main.head ?? 0,
      };
    },
    replaceFormatted(text, cursorOffset) {
      const v = view();
      if (!v) return;
      const change = minimalChange(v.state.doc.toString(), text);
      if (!change) return;
      v.dispatch({
        changes: change,
        selection: cursorOffset == null ? undefined : { anchor: cursorOffset },
        userEvent: 'input.format',
        annotations: isolateHistory.of('full'),
      });
    },
  };
}

/** What a failed format request tells the user. */
export type FormatFailure = {
  /** the toast's title */
  title: string;
  /** the toast's detail */
  detail?: string;
  /** the line (1-based) and column of a syntax error, to highlight */
  line?: number;
  column?: number;
};

/**
 * The message for a failed format request of `what` ("this query", "these shapes"): a
 * syntax error with its line, or the formatter's refusal of its own output. `null` for
 * any other error, which is reported as it came.
 */
export function formatFailure(e: unknown, what: string): FormatFailure | null {
  if (e instanceof ApiError && e.status === 400 && e.code === 'syntax') {
    return {
      title:
        e.line != null
          ? `Can't format: syntax error at line ${e.line}`
          : "Can't format: syntax error",
      detail: e.message,
      line: e.line,
      column: e.column,
    };
  }
  if (e instanceof ApiError && e.status === 422) {
    return { title: `The formatter could not format ${what} safely; it was left unchanged` };
  }
  return null;
}
