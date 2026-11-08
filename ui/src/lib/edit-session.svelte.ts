// The editing session of the ontology and thesaurus editors (docs/specs/
// U12-ontology-workbench/SPEC.md): the preview dialog's state, the undo and redo stacks of
// the session's commits, the bookkeeping after a commit or a refused stale edit, the live
// refresh from the change feed, the keyboard shortcuts and the start of a session branch.
// An edit is previewed as a dry run and committed with `If-Match` by OntoPreview, and an
// undo or a redo is a revert of the commit it takes back.
import * as api from './api';
import { toasts } from './app.svelte';
import { MAIN, validBranchName } from './branches';
import { followCommits } from './changes-feed';
import {
  changedAnything,
  committedSeq,
  sessionBranchName,
  UndoStack,
  type Op,
  type OpAnswer,
} from './ontology';

type Pending = { mode: 'edit' | 'undo' | 'redo'; label: string; after?: () => void };

export class EditSession {
  undo = $state(new UndoStack());
  /** Bumped when the stacks change, since the stack object itself is not reactive. */
  tick = $state(0);
  previewOpen = $state(false);
  previewOp = $state<Op | null>(null);
  previewTitle = $state('');
  live = $state<'off' | 'on' | 'stopped'>('off');
  /** The last commit by another writer, as a sentence. */
  notice = $state<string | null>(null);
  /** The commit that `notice` announces, which a slower answer may show to be this session's. */
  private noticeSeq: number | null = null;
  /** The commits this session made, which live refresh does not announce. */
  readonly own = new Set<number>();
  private pending: Pending = { mode: 'edit', label: '' };

  /**
   * `reload` reads the views again after a commit, a refused stale edit or another
   * writer's commit. `staleText` is the toast of a refused stale edit.
   */
  constructor(
    private readonly opts: {
      reload: () => Promise<void>;
      staleText?: string;
    },
  ) {}

  get done() {
    void this.tick;
    return this.undo.done;
  }
  get undone() {
    void this.tick;
    return this.undo.undone;
  }

  /** A new target or graph: its own stacks, kept under `key`, and no open preview. */
  reset(key: string) {
    this.previewOpen = false;
    this.undo = new UndoStack(key);
    this.tick++;
  }

  /** Preview `op`; `after` runs once it commits. */
  edit(op: Op, label: string, after?: () => void) {
    this.pending = { mode: 'edit', label, after };
    this.previewOp = op;
    this.previewTitle = label;
    this.previewOpen = true;
  }

  undoLast() {
    const top = this.undo.done.at(-1);
    if (!top) return;
    this.pending = { mode: 'undo', label: top.label };
    this.previewOp = { kind: 'revert', commit: top.commit };
    this.previewTitle = `Undo: ${top.label}`;
    this.previewOpen = true;
  }

  redoLast() {
    const top = this.undo.undone.at(-1);
    if (!top) return;
    this.pending = { mode: 'redo', label: top.label };
    this.previewOp = { kind: 'revert', commit: top.commit };
    this.previewTitle = `Redo: ${top.label}`;
    this.previewOpen = true;
  }

  /** A commit made outside the preview, such as a form's save, which undo can take back. */
  record(seq: number | null, label: string) {
    if (seq == null) return;
    this.own.add(seq);
    this.undo.push({ label, commit: seq });
    this.tick++;
  }

  /** The preview committed `op` with answer `a`. */
  async committed(op: Op, a: OpAnswer) {
    void op;
    const seq = committedSeq(a);
    if (seq != null) this.own.add(seq);
    // the change feed announced this commit before its answer arrived
    if (seq != null && this.noticeSeq === seq) {
      this.notice = null;
      this.noticeSeq = null;
    }
    const p = this.pending;
    if (p.mode === 'undo') this.undo.undid(seq);
    else if (p.mode === 'redo') this.undo.redid(seq);
    else if (seq != null) this.undo.push({ label: p.label, commit: seq });
    this.tick++;
    if (changedAnything(a)) {
      toasts.push(
        'success',
        p.mode === 'edit' ? p.label : `${p.mode === 'undo' ? 'Undid' : 'Redid'}: ${p.label}`,
        seq != null ? `Commit ${seq}` : undefined,
      );
    } else toasts.push('info', 'The edit changed nothing.');
    p.after?.();
    await this.opts.reload();
  }

  /** The preview found that another commit landed since the views were read. */
  async stale() {
    toasts.push(
      'info',
      this.opts.staleText ?? 'Another commit landed since the editor read the data.',
      'The views now show the new state.',
    );
    await this.opts.reload();
  }

  /**
   * Follow the change feed of `target` from the commit the views show, once `ready()` says
   * they show `target`. A commit after `shown()` refreshes the views, and one this session
   * did not make sets `notice`. A stream that ends is opened again after a pause. Returns
   * the function that stops it.
   */
  follow(target: string, shown: () => number, ready: () => boolean): () => void {
    const ctl = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));
    const run = async () => {
      // wait for this target's first read, so the stream starts at the commit it shows
      while (!ctl.signal.aborted && !ready()) await pause(200);
      let after = shown();
      let first = true;
      while (!ctl.signal.aborted) {
        if (!first) await pause(1000);
        first = false;
        if (ctl.signal.aborted) return;
        this.live = 'on';
        try {
          after = await followCommits(
            target,
            after,
            (c) => {
              if (c.seq <= shown()) return;
              if (!this.own.has(c.seq)) {
                this.notice = `Commit ${c.seq}${c.message ? ` (${c.message})` : ''} by another writer`;
                this.noticeSeq = c.seq;
              }
              clearTimeout(timer);
              timer = setTimeout(() => void this.opts.reload(), 300);
            },
            ctl.signal,
          );
        } catch {
          if (!ctl.signal.aborted) this.live = 'stopped';
          return;
        }
      }
    };
    void run();
    return () => {
      ctl.abort();
      clearTimeout(timer);
      this.live = 'off';
    };
  }

  /** Ctrl+Z and Ctrl+Shift+Z outside text fields, unless `readonly()`. Returns the removal. */
  keys(readonly: () => boolean): () => void {
    const key = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable))
        return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z' && !readonly()) {
        e.preventDefault();
        if (e.shiftKey) this.redoLast();
        else this.undoLast();
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }
}

/**
 * Create the caller's session branch of today from `from`, unless it exists, and answer its
 * name: `{prefix}.{user}.{yyyymmdd}`.
 */
export async function startSessionBranch(
  ds: string,
  o: {
    from: string | null;
    branches: api.Branch[];
    user: string | null | undefined;
    prefix: string;
    note: string;
  },
): Promise<string> {
  const n = sessionBranchName(o.user, new Date(), o.prefix);
  if (!validBranchName(n)) throw new Error(`Cannot name a branch ${n}.`);
  if (!o.branches.some((b) => b.name === n))
    await api.createBranch(ds, { name: n, from: o.from ?? MAIN, note: o.note });
  toasts.push('success', `Editing on branch ${n}`);
  return n;
}
