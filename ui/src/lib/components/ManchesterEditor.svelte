<script lang="ts">
  // A CodeMirror field for Manchester syntax (docs/specs/U12-ontology-workbench/SPEC.md):
  // completion of the ontology's entity names and of the syntax's keywords, a wavy
  // underline under the span of a problem the server's reader reports, and Enter to
  // submit. A single-line field keeps to one line, and a multi-line one takes
  // Shift+Enter for a line break.
  import {
    autocompletion,
    closeBrackets,
    closeBracketsKeymap,
    completionKeymap,
    completionStatus,
    type CompletionContext,
    type CompletionResult,
  } from '@codemirror/autocomplete';
  import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
  import { EditorState, Prec, type Extension } from '@codemirror/state';
  import { EditorView, keymap, placeholder as cmPlaceholder } from '@codemirror/view';
  import { onDestroy, onMount } from 'svelte';
  import { lintView, setLintDiagnostics } from '$lib/lint-view';
  import { completionWord, manchesterCompletions, type NameOption } from '$lib/manchester-complete';

  let {
    value = $bindable(''),
    names,
    search,
    label,
    placeholder = '',
    multiline = false,
    problem = null,
    onsubmit,
  }: {
    value?: string;
    /** The entity names to complete, with their IRIs and kinds. */
    names: () => NameOption[];
    /**
     * More names found by the server as a name is typed, for an ontology too large to list
     * in the page. They are added to `names()` in the completions.
     */
    search?: (word: string, signal: AbortSignal) => Promise<NameOption[]>;
    /** The accessible name of the field. */
    label: string;
    placeholder?: string;
    multiline?: boolean;
    /** A problem to underline: its span in UTF-16 code units and its message. */
    problem?: { start: number; end: number; message: string } | null;
    onsubmit?: () => void;
  } = $props();

  let host: HTMLDivElement;
  let view: EditorView | undefined;

  const theme = EditorView.theme({
    '&': {
      fontSize: 'var(--fs-sm)',
      backgroundColor: 'var(--surface)',
      color: 'var(--text)',
    },
    '.cm-scroller': { fontFamily: 'var(--font-mono)', lineHeight: '1.5' },
    '.cm-content': { caretColor: 'var(--spark)', padding: '4px 0' },
    '.cm-line': { padding: '0 8px' },
    '.cm-cursor': { borderLeftColor: 'var(--spark)', borderLeftWidth: '2px' },
    '&.cm-focused': { outline: 'none' },
    '.cm-placeholder': { color: 'var(--text-3)', fontFamily: 'var(--font-ui)' },
    '.cm-tooltip': {
      backgroundColor: 'var(--surface)',
      color: 'var(--text)',
      border: '1px solid var(--border)',
      borderRadius: 'var(--r)',
    },
    '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
      backgroundColor: 'var(--spark-soft)',
      color: 'var(--text)',
    },
    '.cm-completionDetail': { color: 'var(--text-3)', marginLeft: '8px', fontStyle: 'normal' },
  });

  async function complete(ctx: CompletionContext): Promise<CompletionResult | null> {
    const line = ctx.state.doc.lineAt(ctx.pos);
    const w = completionWord(line.text.slice(0, ctx.pos - line.from));
    if (!w && !ctx.explicit) return null;
    const from = ctx.pos - (w?.length ?? 0);
    let all = names();
    const typed = (w ?? '').replace(/^'/, '');
    if (search && typed.length >= 2) {
      // the editor drops this request when the word changes, which aborts the search
      const ctl = new AbortController();
      ctx.addEventListener('abort', () => ctl.abort());
      // a pause for typing keeps a search to one request a word
      await new Promise((r) => setTimeout(r, 150));
      if (ctx.aborted) return null;
      try {
        const found = await search(typed, ctl.signal);
        if (ctx.aborted) return null;
        all = [...found, ...all];
      } catch {
        if (ctx.aborted) return null;
      }
    }
    const options = manchesterCompletions(w ?? '', all);
    if (!options.length) return null;
    return { from, options, filter: false };
  }

  function extensions(): Extension {
    return [
      Prec.highest(
        keymap.of([
          {
            key: 'Enter',
            // Enter takes an open completion, and submits otherwise
            run: (v: EditorView) => {
              if (completionStatus(v.state) === 'active') return false;
              onsubmit?.();
              return true;
            },
          },
          ...(multiline
            ? [
                {
                  key: 'Shift-Enter',
                  run: (v: EditorView) => {
                    v.dispatch(v.state.replaceSelection('\n'));
                    return true;
                  },
                },
              ]
            : []),
        ]),
      ),
      history(),
      closeBrackets(),
      autocompletion({ override: [complete], icons: false, activateOnTyping: true }),
      keymap.of([...closeBracketsKeymap, ...completionKeymap, ...defaultKeymap, ...historyKeymap]),
      lintView(),
      cmPlaceholder(placeholder),
      multiline ? [] : EditorState.transactionFilter.of((tr) => (tr.newDoc.lines > 1 ? [] : tr)),
      EditorView.lineWrapping,
      EditorView.contentAttributes.of({
        'aria-label': label,
        spellcheck: 'false',
        role: 'textbox',
        'aria-multiline': multiline ? 'true' : 'false',
      }),
      theme,
      EditorView.updateListener.of((u) => {
        if (u.docChanged) value = u.state.doc.toString();
      }),
    ];
  }

  onMount(() => {
    view = new EditorView({
      state: EditorState.create({ doc: value, extensions: extensions() }),
      parent: host,
    });
  });

  onDestroy(() => view?.destroy());

  // a value set from outside, such as a cleared field, replaces the document
  $effect(() => {
    const v = value;
    if (view && view.state.doc.toString() !== v)
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
  });

  $effect(() => {
    const p = problem;
    if (!view) return;
    // an empty span, such as the end of the text where more was expected, marks the
    // character beside it
    const len = view.state.doc.length;
    let from = Math.min(p?.start ?? 0, len);
    let to = Math.min(Math.max(p?.end ?? 0, from), len);
    if (from === to) {
      if (to < len) to += 1;
      else from = Math.max(0, from - 1);
    }
    view.dispatch({
      effects: setLintDiagnostics.of(
        p
          ? [
              {
                rule: 'manchester',
                severity: 'error',
                message: p.message,
                line: 0,
                column: 0,
                endLine: 0,
                endColumn: 0,
                from,
                to,
              },
            ]
          : [],
      ),
    });
  });

  /** Put the cursor in the field. */
  export function focus() {
    view?.focus();
  }
</script>

<div class="mfield" class:multi={multiline} class:bad={!!problem} bind:this={host}></div>

<style>
  .mfield {
    flex: 1 1 12em;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
  }
  .mfield.multi {
    min-height: 64px;
  }
  .mfield.bad {
    border-color: var(--danger);
  }
  .mfield:focus-within {
    border-color: var(--iri);
    box-shadow: 0 0 0 3px rgba(36, 89, 199, 0.18);
  }
</style>
