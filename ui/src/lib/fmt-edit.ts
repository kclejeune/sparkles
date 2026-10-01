// Applying the formatter's result to an editor: the smallest change that turns the text
// into the formatted text (so undo, scrolling and marks elsewhere are disturbed as little
// as possible), the cursor the server mapped, and a guard that drops a result when the
// text changed while the request was out.

import type { FormatRequest, FormatResult } from './api';

/** One replacement: `[from, to)` of the old text becomes `insert` (UTF-16 offsets). */
export type Change = { from: number; to: number; insert: string };

const isHigh = (c: number) => c >= 0xd800 && c <= 0xdbff;
const isLow = (c: number) => c >= 0xdc00 && c <= 0xdfff;

/**
 * The smallest single change from `before` to `after`: their common prefix and suffix
 * trimmed, never splitting a surrogate pair. `null` when they are equal.
 */
export function minimalChange(before: string, after: string): Change | null {
  if (before === after) return null;
  const max = Math.min(before.length, after.length);
  let start = 0;
  while (start < max && before.charCodeAt(start) === after.charCodeAt(start)) start++;
  // do not end the prefix between the two halves of a pair
  if (start > 0 && (isLow(before.charCodeAt(start)) || isLow(after.charCodeAt(start)))) start--;
  let end = 0;
  while (
    end < max - start &&
    before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)
  )
    end++;
  // nor start the suffix there
  if (
    end > 0 &&
    (isHigh(before.charCodeAt(before.length - end - 1)) ||
      isHigh(after.charCodeAt(after.length - end - 1)))
  )
    end--;
  return {
    from: start,
    to: before.length - end,
    insert: after.slice(start, after.length - end),
  };
}

/**
 * Where the cursor goes in `text`: the server's mapped offset, clamped to the text and
 * moved off the middle of a surrogate pair; `null` when the server mapped none.
 */
export function placeCursor(text: string, cursor: number | null | undefined): number | null {
  if (cursor == null || !Number.isFinite(cursor)) return null;
  let c = Math.max(0, Math.min(Math.trunc(cursor), text.length));
  if (c > 0 && c < text.length && isLow(text.charCodeAt(c)) && isHigh(text.charCodeAt(c - 1))) c--;
  return c;
}

/** What the editor offers the formatting flow. */
export type FormatTarget = {
  /** the text and the cursor to send */
  snapshot(): { text: string; cursorOffset: number };
  /** replace the text with the formatted one in one undoable step */
  replaceFormatted(text: string, cursorOffset: number | null): void;
};

/** How a format request ended. */
export type FormatOutcome =
  /** the editor now holds the formatted text */
  | 'formatted'
  /** the text was formatted already */
  | 'unchanged'
  /** the text changed while the request was out; the result was dropped */
  | 'stale';

/**
 * Format the target's text: snapshot it, ask the server, and apply the result only when
 * the text is still what was sent. Errors (an `ApiError` for a syntax error, a refusal)
 * propagate to the caller.
 */
export async function formatEditor(
  target: FormatTarget,
  request: (req: FormatRequest) => Promise<FormatResult>,
): Promise<FormatOutcome> {
  const { text, cursorOffset } = target.snapshot();
  const result = await request({ text, language: 'sparql', cursorOffset });
  if (target.snapshot().text !== text) return 'stale';
  if (result.text === text) return 'unchanged';
  target.replaceFormatted(result.text, placeCursor(result.text, result.cursorOffset));
  return 'formatted';
}
