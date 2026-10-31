import type { LintDiagnostic } from './api';

/**
 * Lint an editor's text a moment after it stops changing, and show only the findings of
 * the text that is there now. Every call of `schedule` starts a new generation, so a
 * request still pending for older text is dropped when it answers, and so is one whose
 * editor was cleaned up. A `null` request (a blank editor, linting turned off) clears the
 * findings at once.
 */
export function lintRunner<R>(opts: {
  delay: number;
  run: (req: R) => Promise<LintDiagnostic[]>;
  show: (found: LintDiagnostic[]) => void;
}): { schedule: (req: R | null) => () => void } {
  let generation = 0;
  return {
    schedule(req) {
      const mine = ++generation;
      const drop = () => {
        if (generation === mine) generation++;
      };
      if (req == null) {
        opts.show([]);
        return drop;
      }
      const timer = setTimeout(async () => {
        let found: LintDiagnostic[] = [];
        try {
          found = await opts.run(req);
        } catch {
          found = [];
        }
        if (generation === mine) opts.show(found);
      }, opts.delay);
      return () => {
        clearTimeout(timer);
        drop();
      };
    },
  };
}
