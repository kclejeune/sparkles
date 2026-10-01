import { history, redo, undo } from '@codemirror/commands';
import { EditorState, Transaction, type TransactionSpec } from '@codemirror/state';
import { describe, expect, it } from 'vitest';
import { ApiError, type FormatRequest } from './api';
import { formatEditor } from './fmt-edit';
import { formatFailure, viewTarget, type ViewLike } from './fmt-view';

/** An editor without a DOM: its state, and a dispatch that applies transactions (the
 * history commands dispatch a `Transaction`, the target a spec). */
function fakeView(doc: string, cursor = 0): ViewLike & { state: EditorState } {
  const v = {
    state: EditorState.create({ doc, selection: { anchor: cursor }, extensions: [history()] }),
    dispatch(tr: TransactionSpec | Transaction) {
      v.state = tr instanceof Transaction ? tr.state : v.state.update(tr).state;
    },
  };
  return v;
}

/** Type a change by hand, as its own undo step. */
function type(v: ReturnType<typeof fakeView>, at: number, text: string) {
  v.dispatch({ changes: { from: at, insert: text }, userEvent: 'input.type' });
}

describe('viewTarget', () => {
  it('snapshots the text and the cursor', () => {
    const v = fakeView('ex:a ex:p ex:b .', 5);
    expect(viewTarget(() => v).snapshot()).toEqual({ text: 'ex:a ex:p ex:b .', cursorOffset: 5 });
    expect(viewTarget(() => undefined).snapshot()).toEqual({ text: '', cursorOffset: 0 });
  });

  it('replaces the changed middle in one step that undo reverts alone', () => {
    const v = fakeView('');
    type(v, 0, 'ex:a   ex:p ex:b .');
    const target = viewTarget(() => v);
    target.replaceFormatted('ex:a ex:p ex:b .\n', 4);
    expect(v.state.doc.toString()).toBe('ex:a ex:p ex:b .\n');
    expect(v.state.selection.main.head).toBe(4);

    expect(undo(v)).toBe(true);
    expect(v.state.doc.toString()).toBe('ex:a   ex:p ex:b .');
    expect(redo(v)).toBe(true);
    expect(v.state.doc.toString()).toBe('ex:a ex:p ex:b .\n');
    // the typing before it is its own step
    undo(v);
    undo(v);
    expect(v.state.doc.toString()).toBe('');
  });

  it('keeps the selection mapped without a cursor, and does nothing without a change', () => {
    const v = fakeView('a  b\nc', 6);
    const target = viewTarget(() => v);
    target.replaceFormatted('a b\nc', null);
    expect(v.state.doc.toString()).toBe('a b\nc');
    expect(v.state.selection.main.head).toBe(5);
    const before = v.state;
    target.replaceFormatted('a b\nc', 0);
    expect(v.state).toBe(before);
  });

  it('formats through formatEditor with the language the caller sets', async () => {
    const v = fakeView('ex:a  ex:p  ex:b .', 2);
    const sent: FormatRequest[] = [];
    const outcome = await formatEditor(
      viewTarget(() => v),
      async (req) => {
        sent.push({ ...req, language: 'turtle' });
        return {
          text: 'ex:a ex:p ex:b .\n',
          changed: true,
          language: 'turtle',
          cursorOffset: 2,
          warnings: [],
        };
      },
    );
    expect(outcome).toBe('formatted');
    expect(sent).toEqual([{ text: 'ex:a  ex:p  ex:b .', language: 'turtle', cursorOffset: 2 }]);
    expect(v.state.doc.toString()).toBe('ex:a ex:p ex:b .\n');
  });
});

describe('formatFailure', () => {
  it('names the line of a syntax error', () => {
    const e = new ApiError(400, 'expected a term', { code: 'syntax', line: 3, column: 7 });
    expect(formatFailure(e, 'these shapes')).toEqual({
      title: "Can't format: syntax error at line 3",
      detail: 'expected a term',
      line: 3,
      column: 7,
    });
    const nowhere = new ApiError(400, 'bad', { code: 'syntax' });
    expect(formatFailure(nowhere, 'these shapes')?.title).toBe("Can't format: syntax error");
  });

  it('reports a refusal, and leaves other errors to the caller', () => {
    const refused = new ApiError(422, 'graph differs', { code: 'unsafe-format' });
    expect(formatFailure(refused, 'these shapes')).toEqual({
      title: 'The formatter could not format these shapes safely; it was left unchanged',
    });
    expect(formatFailure(new ApiError(415, 'not available'), 'these shapes')).toBeNull();
    expect(formatFailure(new ApiError(400, 'bad option', { code: 'bad-request' }), 'x')).toBeNull();
    expect(formatFailure(new Error('offline'), 'x')).toBeNull();
  });
});
