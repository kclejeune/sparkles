<script lang="ts">
  // A small CodeMirror editor for a Turtle document (the SHACL shapes graph): the SPARQL
  // tokenizer highlights it (the two share their terms), with undo history, the error
  // line of a syntax error, and Shift+Alt+F to format.
  import { closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete';
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
    toggleComment,
  } from '@codemirror/commands';
  import { bracketMatching, syntaxHighlighting } from '@codemirror/language';
  import { searchKeymap } from '@codemirror/search';
  import { EditorState, Prec, type Extension } from '@codemirror/state';
  import { drawSelection, EditorView, keymap, lineNumbers } from '@codemirror/view';
  import { onDestroy, onMount } from 'svelte';
  import { viewTarget } from '$lib/fmt-view';
  import { errorField, highlightError, sparqlHighlight, sparqlLanguage } from '$lib/sparql-lang';

  let {
    value,
    onchange,
    onformat,
    label,
  }: {
    value: string;
    onchange: (value: string) => void;
    /** Format the document (Shift+Alt+F). */
    onformat?: () => void;
    /** The accessible name of the text box. */
    label: string;
  } = $props();

  let host: HTMLDivElement;
  let view: EditorView | undefined;

  const theme = EditorView.theme({
    '&': {
      height: '100%',
      fontSize: '12px',
      backgroundColor: 'var(--surface)',
      color: 'var(--text)',
    },
    '.cm-scroller': { fontFamily: 'var(--font-mono)', lineHeight: '1.5' },
    '.cm-content': { caretColor: 'var(--spark)', padding: '6px 0' },
    '.cm-cursor': { borderLeftColor: 'var(--spark)', borderLeftWidth: '2px' },
    '.cm-gutters': {
      backgroundColor: 'var(--surface)',
      color: 'var(--text-3)',
      border: 'none',
    },
    '&.cm-focused': { outline: 'none' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection': {
      backgroundColor: 'color-mix(in srgb, var(--iri) 22%, transparent) !important',
    },
    '.cm-matchingBracket': {
      backgroundColor: 'var(--spark-soft)',
      outline: '1px solid var(--spark)',
    },
    '.cm-error-line': {
      backgroundColor: 'var(--danger-soft)',
      boxShadow: 'inset 3px 0 0 var(--danger)',
    },
    '.cm-error-mark': {
      textDecoration: 'underline wavy var(--danger)',
      textUnderlineOffset: '3px',
    },
    '.cm-panels': {
      backgroundColor: 'var(--surface-2)',
      color: 'var(--text)',
      borderColor: 'var(--border)',
    },
  });

  function extensions(): Extension {
    return [
      Prec.highest(
        keymap.of([
          {
            key: 'Shift-Alt-f',
            run: () => (onformat ? (onformat(), true) : false),
            preventDefault: true,
          },
          { key: 'Mod-/', run: toggleComment },
        ]),
      ),
      lineNumbers(),
      history(),
      drawSelection(),
      bracketMatching(),
      closeBrackets(),
      keymap.of([
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        indentWithTab,
      ]),
      sparqlLanguage,
      syntaxHighlighting(sparqlHighlight),
      errorField,
      EditorState.tabSize.of(2),
      EditorView.contentAttributes.of({ 'aria-label': label, spellcheck: 'false' }),
      theme,
      EditorView.updateListener.of((u) => {
        if (u.docChanged) onchange(u.state.doc.toString());
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

  // a value set from outside (another dataset's draft) starts a fresh document and history
  $effect(() => {
    const v = value;
    if (view && view.state.doc.toString() !== v)
      view.setState(EditorState.create({ doc: v, extensions: extensions() }));
  });

  const target = viewTarget(() => view);

  /** The document and the cursor (UTF-16 code units), to format. */
  export function snapshot(): { text: string; cursorOffset: number } {
    return target.snapshot();
  }

  /** Replace the document with its formatted text in one undoable step. */
  export function replaceFormatted(text: string, cursorOffset: number | null) {
    target.replaceFormatted(text, cursorOffset);
  }

  /** Highlight the line (and column) of a syntax error; no line clears it. */
  export function showError(line?: number, column?: number) {
    if (view) highlightError(view, line, column);
  }
</script>

<div class="editor" bind:this={host}></div>

<style>
  .editor {
    height: 240px;
    min-height: 80px;
    resize: vertical;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
  }
  .editor:focus-within {
    border-color: var(--iri);
    box-shadow: 0 0 0 3px rgba(36, 89, 199, 0.18);
  }
  .editor :global(.cm-editor) {
    height: 100%;
  }
</style>
