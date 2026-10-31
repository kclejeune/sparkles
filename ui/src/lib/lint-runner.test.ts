import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LintDiagnostic } from './api';
import { lintRunner } from './lint-runner';

const finding = (message: string): LintDiagnostic =>
  ({
    rule: 'unused-prefix',
    severity: 'warning',
    message,
    line: 1,
    column: 1,
    endLine: 1,
    endColumn: 2,
    from: 0,
    to: 1,
  }) as LintDiagnostic;

/** A linter whose answers the test releases by hand. */
function deferred() {
  const pending: { req: string; resolve: (d: LintDiagnostic[]) => void }[] = [];
  const run = (req: string) =>
    new Promise<LintDiagnostic[]>((resolve) => pending.push({ req, resolve }));
  return { pending, run };
}

describe('lint runner', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('drops an answer that arrives after the editor was cleared', async () => {
    const lint = deferred();
    const shown: LintDiagnostic[][] = [];
    const r = lintRunner({ delay: 400, run: lint.run, show: (d) => shown.push(d) });
    const cleanup = r.schedule('PREFIX u: <u:> ASK {}');
    await vi.advanceTimersByTimeAsync(400);
    expect(lint.pending).toHaveLength(1);
    // the user clears the editor while the request is pending
    cleanup();
    r.schedule(null);
    expect(shown).toEqual([[]]);
    lint.pending[0].resolve([finding('stale')]);
    await vi.runAllTimersAsync();
    expect(shown).toEqual([[]]);
  });

  it('drops an answer for text that has changed since', async () => {
    const lint = deferred();
    const shown: LintDiagnostic[][] = [];
    const r = lintRunner({ delay: 400, run: lint.run, show: (d) => shown.push(d) });
    const first = r.schedule('one');
    await vi.advanceTimersByTimeAsync(400);
    // an edit: the effect cleans up and schedules the new text
    first();
    r.schedule('two');
    lint.pending[0].resolve([finding('stale')]);
    await vi.advanceTimersByTimeAsync(0);
    expect(shown).toEqual([]);
    await vi.advanceTimersByTimeAsync(400);
    expect(lint.pending.map((p) => p.req)).toEqual(['one', 'two']);
    lint.pending[1].resolve([finding('fresh')]);
    await vi.advanceTimersByTimeAsync(0);
    expect(shown).toEqual([[finding('fresh')]]);
  });

  it('drops an answer once the editor is cleaned up, and lints nothing pending', async () => {
    const lint = deferred();
    const shown: LintDiagnostic[][] = [];
    const r = lintRunner({ delay: 400, run: lint.run, show: (d) => shown.push(d) });
    r.schedule('one')();
    await vi.advanceTimersByTimeAsync(400);
    expect(lint.pending).toHaveLength(0);
    const cleanup = r.schedule('two');
    await vi.advanceTimersByTimeAsync(400);
    cleanup();
    lint.pending[0].resolve([finding('stale')]);
    await vi.advanceTimersByTimeAsync(0);
    expect(shown).toEqual([]);
  });

  it('shows nothing when the linter fails', async () => {
    const shown: LintDiagnostic[][] = [];
    const r = lintRunner<string>({
      delay: 10,
      run: () => Promise.reject(new Error('down')),
      show: (d) => shown.push(d),
    });
    r.schedule('x');
    await vi.advanceTimersByTimeAsync(10);
    expect(shown).toEqual([[]]);
  });
});
