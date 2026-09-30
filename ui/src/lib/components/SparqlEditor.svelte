<script lang="ts">
  import { closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete';
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
    toggleComment,
  } from '@codemirror/commands';
  import { bracketMatching, indentOnInput } from '@codemirror/language';
  import { highlightSelectionMatches, searchKeymap } from '@codemirror/search';
  import { EditorState, Prec, type Extension } from '@codemirror/state';
  import {
    drawSelection,
    EditorView,
    highlightActiveLine,
    highlightActiveLineGutter,
    keymap,
    lineNumbers,
    placeholder as cmPlaceholder,
  } from '@codemirror/view';
  import { onDestroy, onMount } from 'svelte';
  import { highlightError, sparql, type CompletionData } from '$lib/sparql-lang';

  let {
    docId,
    value,
    onchange,
    onrun,
    completion,
    placeholder = 'SELECT * WHERE { ?s ?p ?o } LIMIT 100',
  }: {
    /** Identity of the document; switching it swaps editor state (keeps per-tab undo history). */
    docId: string;
    value: string;
    onchange: (value: string) => void;
    onrun: () => void;
    completion: CompletionData;
    placeholder?: string;
  } = $props();

  let host: HTMLDivElement;
  let view: EditorView | undefined;
  const states = new Map<string, EditorState>();
  let currentId = '';

  const theme = EditorView.theme({
    '&': {
      height: '100%',
      fontSize: '13px',
      backgroundColor: 'var(--surface)',
      color: 'var(--text)',
    },
    '.cm-scroller': { fontFamily: 'var(--font-mono)', lineHeight: '1.6' },
    '.cm-content': { caretColor: 'var(--spark)', padding: '8px 0' },
    '.cm-cursor': { borderLeftColor: 'var(--spark)', borderLeftWidth: '2px' },
    '.cm-gutters': {
      backgroundColor: 'var(--surface)',
      color: 'var(--text-3)',
      border: 'none',
      paddingLeft: '4px',
    },
    '.cm-activeLineGutter': { backgroundColor: 'transparent', color: 'var(--text)' },
    '.cm-activeLine': { backgroundColor: 'var(--hover)' },
    '&.cm-focused': { outline: 'none' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection': {
      backgroundColor: 'color-mix(in srgb, var(--iri) 22%, transparent) !important',
    },
    '.cm-matchingBracket': {
      backgroundColor: 'var(--spark-soft)',
      outline: '1px solid var(--spark)',
    },
    '.cm-selectionMatch': { backgroundColor: 'color-mix(in srgb, var(--iri) 12%, transparent)' },
    '.cm-error-line': {
      backgroundColor: 'var(--danger-soft)',
      boxShadow: 'inset 3px 0 0 var(--danger)',
    },
    '.cm-error-mark': {
      textDecoration: 'underline wavy var(--danger)',
      textUnderlineOffset: '3px',
    },
    '.cm-placeholder': { color: 'var(--text-3)' },
    '.cm-tooltip': {
      backgroundColor: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: '6px',
      boxShadow: 'var(--shadow-pop)',
      overflow: 'hidden',
    },
    '.cm-tooltip-autocomplete > ul': {
      fontFamily: 'var(--font-mono)',
      fontSize: '12px',
      maxHeight: '260px',
    },
    '.cm-tooltip-autocomplete > ul > li': { padding: '2px 8px !important', lineHeight: '1.6' },
    '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
      backgroundColor: 'color-mix(in srgb, var(--iri) 18%, transparent)',
      color: 'var(--text)',
    },
    '.cm-completionDetail': {
      color: 'var(--text-3)',
      fontStyle: 'normal',
      marginLeft: '12px',
      fontSize: '11px',
    },
    '.cm-completionIcon': { opacity: '0.55', width: '1em' },
    '.cm-panels': {
      backgroundColor: 'var(--surface-2)',
      color: 'var(--text)',
      borderColor: 'var(--border)',
    },
    '.cm-panel input, .cm-panel button': { fontFamily: 'var(--font-ui)' },
    '.cm-textfield': {
      backgroundColor: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: '4px',
      color: 'var(--text)',
    },
    '.cm-button': {
      backgroundImage: 'none',
      backgroundColor: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: '4px',
      color: 'var(--text)',
    },
  });

  function extensions(): Extension {
    return [
      Prec.highest(
        keymap.of([
          { key: 'Mod-Enter', run: () => (onrun(), true), preventDefault: true },
          { key: 'Mod-/', run: toggleComment },
        ]),
      ),
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightActiveLine(),
      history(),
      drawSelection(),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      highlightSelectionMatches(),
      cmPlaceholder(placeholder),
      keymap.of([
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...completionKeymap,
        indentWithTab,
      ]),
      sparql(completion),
      theme,
      EditorView.updateListener.of((u) => {
        if (u.docChanged) onchange(u.state.doc.toString());
      }),
    ];
  }

  function stateFor(id: string, doc: string): EditorState {
    return states.get(id) ?? EditorState.create({ doc, extensions: extensions() });
  }

  onMount(() => {
    currentId = docId;
    view = new EditorView({ state: stateFor(docId, value), parent: host });
  });

  onDestroy(() => view?.destroy());

  // Swap documents when the active tab changes; sync external value edits.
  $effect(() => {
    const id = docId;
    const v = value;
    if (!view) return;
    if (id !== currentId) {
      states.set(currentId, view.state);
      currentId = id;
      const st = stateFor(id, v);
      view.setState(
        st.doc.toString() === v ? st : EditorState.create({ doc: v, extensions: extensions() }),
      );
    } else if (view.state.doc.toString() !== v) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  });

  export function showError(line?: number, column?: number) {
    if (view) highlightError(view, line, column);
  }

  export function focus() {
    view?.focus();
  }

  export function forget(id: string) {
    states.delete(id);
  }
</script>

<div class="editor" bind:this={host}></div>

<style>
  .editor {
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }
  .editor :global(.cm-editor) {
    height: 100%;
  }
</style>
