<script lang="ts" module>
  import { LayoutStore } from '$lib/pattern-layout';

  /** The layout maps of every editor tab, shared by the pane and the result's tab. */
  export const layouts = new LayoutStore();
</script>

<script lang="ts">
  // The Pattern pane of spec U08: the drawing of a query's pattern beside the editor, or
  // of the query that produced a result (`readonly`). The text is the only source of
  // truth. The pane parses it 150 ms after it stops changing, with the browser module of
  // sparkles-fmt, and draws the answer. A canvas edit is a list of changes the page applies
  // to the editor as one transaction, after which the pane redraws from the new text like
  // after any typing. Text that does not parse leaves the last pattern on screen, dimmed,
  // and the pane refuses edits until it parses again.
  import { onDestroy, onMount, tick } from 'svelte';
  import { LocalUnavailable, patternLocal, patternNow } from '$lib/fmt-wasm';
  import {
    agrees,
    announcement,
    applyChanges,
    describe,
    drawing as drawingOf,
    edit as editOf,
    EditError,
    lockedOf,
    MAX_TEXT,
    missingPrefixes,
    overCaps,
    renamed,
    stillNames,
    textOrder,
    type Change,
    type Drawing,
    type Edit,
    type PatternAnswer,
    type PatternError,
    type PatternOk,
  } from '$lib/pattern';
  import { kept, type Layout } from '$lib/pattern-layout';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';
  import PatternCanvas, { type Focusable } from './PatternCanvas.svelte';
  import PatternPanel from './PatternPanel.svelte';

  let {
    text,
    tab = null,
    readonly = false,
    phone = false,
    prefixes = {},
    onedit,
    onundo,
    onredo,
    onescape,
    onclose,
    debounce = 150,
  }: {
    text: string;
    /** the editor tab whose layout the pane keeps; null for a result's pattern */
    tab?: string | null;
    readonly?: boolean;
    phone?: boolean;
    /** the dataset's declared prefixes, for a prefixed name the query does not declare */
    prefixes?: PrefixMap;
    /** apply changes to the editor as one transaction */
    onedit?: (changes: Change[]) => void;
    onundo?: () => void;
    onredo?: () => void;
    /** return focus to the editor */
    onescape?: () => void;
    onclose?: () => void;
    debounce?: number;
  } = $props();

  let answer = $state<PatternOk | null>(null);
  let error = $state<PatternError | null>(null);
  let unavailable = $state<string | null>(null);
  let tooLarge = $state(false);
  let drawn = $state<Drawing | null>(null);
  let layout = $state<Layout>({});
  /** the text the drawing is of */
  let drawnText = $state('');
  let mode = $state<'canvas' | 'list'>('canvas');
  let selected = $state<Focusable | null>(null);
  let adding = $state(false);
  let addS = $state('');
  let addP = $state('');
  let addO = $state('');
  let addError = $state<string | null>(null);
  let live = $state('');
  let canvas: PatternCanvas | undefined = $state();
  let root: HTMLElement;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  /** timings of the last redraw, for the performance test (§4.6) */
  let timing = $state({ parseMs: 0, drawMs: 0 });

  const canEdit = $derived(!readonly && !!answer && !error && !!onedit);
  const over = $derived(!!answer && !!drawn && overCaps(answer, drawn));
  const rows = $derived(drawn ? textOrder(drawn) : []);

  function show(text: string, a: PatternAnswer, parseMs: number) {
    const t0 = performance.now();
    if (!a.ok) {
      error = a;
      return;
    }
    error = null;
    const next = drawingOf(a);
    if (tab && drawn) {
      // a variable renamed in the editor keeps its place, with its literals
      for (const [from, to] of renamed(drawn, next) ?? []) layouts.rename(tab, from, to);
    }
    // the open panel follows its element by id, or closes when it is gone
    if (selected) {
      const id = 'node' in selected ? selected.node.id : selected.edge.id;
      const node = next.nodes.find((n) => n.id === id);
      const edge =
        node || !drawn || !stillNames(drawn, next, id)
          ? undefined
          : next.edges.find((x) => x.id === id);
      selected = node ? { node } : edge ? { edge } : null;
    }
    const map = tab ? layouts.get(tab, text) : (layouts.stored(text) ?? {});
    layout = kept(
      map,
      next.nodes.map((n) => n.id),
    );
    answer = a;
    drawn = next;
    drawnText = text;
    timing = { parseMs, drawMs: performance.now() - t0 };
  }

  async function parse(text: string) {
    const mine = ++seq;
    if (text.length > MAX_TEXT) {
      tooLarge = true;
      return;
    }
    tooLarge = false;
    const t0 = performance.now();
    const now = patternNow(text);
    if (now) return show(text, now, performance.now() - t0);
    try {
      const a = await patternLocal(text);
      if (mine === seq) show(text, a, performance.now() - t0);
      unavailable = null;
    } catch (e) {
      if (!(e instanceof LocalUnavailable)) throw e;
      unavailable = e.message;
    }
  }

  // parse 150 ms after the text stops changing; the first time at once
  let first = true;
  $effect(() => {
    const t = text;
    clearTimeout(timer);
    if (first) {
      first = false;
      void parse(t);
      return;
    }
    timer = setTimeout(() => void parse(t), debounce);
  });

  onDestroy(() => clearTimeout(timer));

  function onlayout(l: Layout) {
    layout = l;
    if (tab) layouts.set(tab, drawnText, l);
  }

  /** Make an edit: the changes go to the editor, which the pane then redraws from. */
  function apply(e: Edit) {
    if (!answer || !canEdit) return;
    // typing since the last redraw: the ranges are of the text as it is now
    if (text !== drawnText) {
      const now = patternNow(text);
      if (!now?.ok) return;
      const prev = drawn;
      show(text, now, 0);
      // the edge was picked in the drawing before the typing
      if (e.kind !== 'add' && (!prev || !drawn || !stillNames(prev, drawn, e.edge))) {
        live = 'The pattern changed, select the edge again';
        return;
      }
    }
    const refuse = (why: string) => {
      live = why;
      if (e.kind === 'add') addError = why;
    };
    let changes: Change[];
    try {
      changes = editOf(drawnText, answer, e);
    } catch (err) {
      if (err instanceof EditError) return refuse(err.message);
      throw err;
    }
    // a canvas edit never lands in text the server would refuse, and the parse of the
    // result must be the pattern the edit describes (R3)
    const next = applyChanges(drawnText, changes);
    const check = patternNow(next, true);
    if (!check) return refuse('The browser module is not ready, so the edit was not made');
    if (!check.ok) return refuse(`That edit would not parse: ${check.error}`);
    if (!agrees(answer, e, check))
      return refuse('That edit would change more of the pattern than it names, so it was not made');
    onedit!(changes);
    live = announcement(e, answer, drawingOf(check));
    selected = null;
    if (e.kind === 'add') {
      addS = addP = addO = '';
      addError = null;
    }
  }

  function addTriple(ev: SubmitEvent) {
    ev.preventDefault();
    // prefixed names the query does not declare, declared for the dataset
    const declare = answer ? missingPrefixes([addS, addP, addO], answer, prefixes) : [];
    apply({
      kind: 'add',
      s: addS,
      p: addP,
      o: addO,
      ...(declare.length ? { declare } : {}),
    });
  }

  async function openAdd() {
    adding = true;
    await tick();
    root?.querySelector<HTMLInputElement>('input[name="pattern-s"]')?.focus();
  }

  /** Undo, redo and Escape anywhere in the pane but its text inputs, the canvas, the list
   * and the panel alike. */
  function paneKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    if (t?.closest('input, textarea, select, [contenteditable="true"]')) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.altKey && (e.key === 'z' || e.key === 'Z')) {
      e.preventDefault();
      if (e.shiftKey) onredo?.();
      else onundo?.();
    } else if (mod && !e.altKey && (e.key === 'y' || e.key === 'Y')) {
      e.preventDefault();
      onredo?.();
    } else if (e.key === 'Escape' && !mod) {
      e.preventDefault();
      if (selected) {
        selected = null;
        canvas?.focus();
      } else onescape?.();
    }
  }

  /** The canvas's own keys on the focused element; the pane's keys bubble to `paneKey`. */
  function key(e: KeyboardEvent, el: Focusable | null) {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const edge = el && 'edge' in el && el.edge.triple ? el.edge.id : undefined;
    if ((e.key === 'Delete' || e.key === 'Backspace') && edge) {
      e.preventDefault();
      apply({ kind: 'delete', edge });
    } else if ((e.key === 'o' || e.key === 'O') && edge) {
      e.preventDefault();
      apply({ kind: 'optional', edge });
    } else if ((e.key === 'n' || e.key === 'N') && canEdit) {
      e.preventDefault();
      void openAdd();
    }
  }

  const lineCol = (e: PatternError) => `line ${e.line}, column ${e.column}`;
  const modifiers = $derived(answer?.modifiers ?? []);

  /** Focus wanted before the first pattern arrived: given when the canvas or list appears. */
  let pendingFocus = $state(false);

  /** Focus the canvas, or the list. */
  export function focus() {
    if (!tryFocus()) pendingFocus = true;
  }

  function tryFocus(): boolean {
    const list = root?.querySelector<HTMLElement>('.plist button');
    if (mode === 'canvas' && canvas) canvas.focus();
    else if (mode === 'list' && list) list.focus();
    else return false;
    return true;
  }

  $effect(() => {
    if (!pendingFocus || !drawn) return;
    void tick().then(() => {
      pendingFocus = false;
      tryFocus();
    });
  });

  onMount(() => {
    root.dataset.ready = 'true';
  });
</script>

<!-- the pane's shortcuts reach it from every control inside; see paneKey -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  class="ppane"
  class:phone
  bind:this={root}
  onkeydown={paneKey}
  aria-label={readonly ? 'Pattern of the query that ran' : 'Pattern'}
  data-parse-ms={timing.parseMs.toFixed(2)}
  data-draw-ms={timing.drawMs.toFixed(2)}
  data-state={unavailable
    ? 'unavailable'
    : error
      ? error.code === 'update'
        ? 'update'
        : 'error'
      : answer
        ? 'ok'
        : 'loading'}
>
  <header class="phead">
    {#if !phone}<strong>Pattern</strong>{/if}
    {#if readonly}<span class="badge">read-only</span>{/if}
    <span class="spacer"></span>
    {#if canEdit}
      <button class="btn sm" onclick={openAdd} title="Add a triple (N)">
        <Icon name="plus" size={13} /> Triple
      </button>
    {/if}
    <div class="seg" role="group" aria-label="Show the pattern as">
      <button class="btn sm" aria-pressed={mode === 'canvas'} onclick={() => (mode = 'canvas')}
        >Canvas</button
      >
      <button class="btn sm" aria-pressed={mode === 'list'} onclick={() => (mode = 'list')}
        >List</button
      >
    </div>
    {#if mode === 'canvas' && drawn}
      <button
        class="btn sm icon"
        title="Fit to view"
        aria-label="Fit to view"
        onclick={() => canvas?.fit()}><Icon name="fit" size={14} /></button
      >
      <button class="btn sm" title="Lay the whole pattern out again" onclick={() => canvas?.tidy()}
        >Tidy</button
      >
    {/if}
    {#if onclose}
      <button
        class="btn ghost icon sm"
        aria-label="Close the Pattern pane"
        title="Close (Alt+P)"
        onclick={onclose}><Icon name="x" size={12} /></button
      >
    {/if}
  </header>

  {#if adding && canEdit}
    <form class="add" onsubmit={addTriple} aria-label="Add a triple">
      <input
        name="pattern-s"
        aria-label="Subject"
        placeholder="?person"
        bind:value={addS}
        required
      />
      <input
        name="pattern-p"
        aria-label="Predicate"
        placeholder="ex:worksFor"
        bind:value={addP}
        required
      />
      <input name="pattern-o" aria-label="Object" placeholder="?org" bind:value={addO} required />
      <button class="btn sm primary" type="submit">Add</button>
      <button
        class="btn sm ghost"
        type="button"
        onclick={() => {
          adding = false;
          addError = null;
        }}>Cancel</button
      >
      {#if addError}<p class="err" role="alert">{addError}</p>{/if}
    </form>
  {/if}

  <div class="pbody">
    {#if unavailable}
      <div class="state">
        <Icon name="alert" size={18} />
        <p>
          The visual query builder needs the browser module of the formatter, which did not load.
        </p>
      </div>
    {:else if tooLarge}
      <div class="state"><p>This query is longer than 256 KiB, more than the pane parses.</p></div>
    {:else if error?.code === 'update' && !answer}
      <div class="state"><p>Updates are not drawn. The builder edits queries.</p></div>
    {:else if error && !answer}
      <div class="state" role="status">
        <p>The query does not parse ({lineCol(error)}).</p>
        <p class="faint small">{error.error}</p>
      </div>
    {:else if answer && drawn}
      {#if error}
        <p class="notice warn" role="status">
          {error.code === 'update'
            ? 'Updates are not drawn. The builder edits queries.'
            : `The text does not parse at ${lineCol(error)}, so this is the last pattern and edits wait until it parses.`}
        </p>
      {:else if over}
        <p class="notice" role="status">
          This pattern has {drawn.triples} triple patterns, more than the canvas draws.
        </p>
      {:else if drawn.nodes.length === 0}
        <p class="notice" role="status">Add a node to start.</p>
      {:else if drawn.parts > 1}
        <p class="notice warn" role="status">
          These parts are not connected, so the query matches every combination.
        </p>
      {/if}
      {#if mode === 'canvas' && !over}
        <div class="canvas-host">
          <PatternCanvas
            bind:this={canvas}
            drawing={drawn}
            {layout}
            dim={!!error}
            {phone}
            selected={selected ? ('node' in selected ? selected.node.id : selected.edge.id) : null}
            onselect={(el) => (selected = el)}
            {onlayout}
            onkey={key}
          />
          {#if selected}
            <PatternPanel
              el={selected}
              drawing={drawn}
              text={drawnText}
              readonly={!canEdit}
              sheet={phone}
              ondelete={(edge) => apply({ kind: 'delete', edge })}
              onoptional={(edge) => apply({ kind: 'optional', edge })}
              onclose={() => (selected = null)}
            />
          {/if}
        </div>
      {:else}
        <ol class="plist" aria-label="Pattern elements in text order">
          {#each rows as row, i (i)}
            {@const triple = 'edge' in row ? row.edge.triple : undefined}
            {@const edge = 'edge' in row ? row.edge.id : ''}
            <li class:locked={'node' in row && !!row.node.locked}>
              <span class="desc">{describe(row, drawn)}</span>
              {#if triple && canEdit}
                <button class="btn sm" onclick={() => apply({ kind: 'optional', edge })}
                  >{triple.path.length > 1 ? 'Required' : 'Optional'}</button
                >
                <button
                  class="btn sm danger"
                  aria-label="Delete {triple.triple.s.text} {triple.triple.p.text} {triple.triple.o
                    .text}"
                  onclick={() => apply({ kind: 'delete', edge })}
                  ><Icon name="trash" size={12} /></button
                >
              {/if}
            </li>
          {/each}
        </ol>
      {/if}
      <div class="strip" aria-label="Projection and modifiers">
        {#if answer.projection}
          <span class="lbl">SELECT</span>
          {#if answer.projection.star}<span class="chip mono">*</span>{/if}
          {#each answer.projection.vars as v (v)}<span class="chip mono">?{v}</span>{/each}
          {#if answer.projection.locked}<span
              class="chip locked"
              title="Expressions in the projection are edited in the editor">locked</span
            >{/if}
          {#if answer.projection.distinct}<span class="chip">DISTINCT</span>{/if}
        {:else}
          <span class="lbl">{answer.form.toUpperCase()}</span>
        {/if}
        {#if answer.limit}<span class="chip mono">LIMIT {answer.limit.value}</span>{/if}
        {#if answer.offset}<span class="chip mono">OFFSET {answer.offset.value}</span>{/if}
        {#each modifiers as m, i (i)}<span class="chip mono locked" title={m.text}>{m.kind}</span
          >{/each}
        <span class="spacer"></span>
        <span class="faint small" data-testid="pattern-counts"
          >{drawn.triples} triples · {lockedOf(answer).length} locked</span
        >
      </div>
    {:else}
      <div class="state"><p class="faint">Reading the pattern…</p></div>
    {/if}
  </div>
  <p class="sr-only" aria-live="polite" data-testid="pattern-live">{live}</p>
</section>

<style>
  .ppane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    height: 100%;
    background: var(--surface);
  }
  .phead {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .spacer {
    flex: 1;
  }
  .seg {
    display: inline-flex;
  }
  .seg .btn[aria-pressed='true'] {
    background: var(--spark-soft);
    border-color: var(--spark);
  }
  .add {
    display: flex;
    gap: 6px;
    padding: 6px 10px;
    flex-wrap: wrap;
    border-bottom: 1px solid var(--border);
  }
  .add input {
    flex: 1 1 110px;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .add .err {
    flex-basis: 100%;
    margin: 0;
    color: var(--danger);
    font-size: 12px;
  }
  .pbody {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .canvas-host {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .state {
    flex: 1;
    display: grid;
    place-items: center;
    align-content: center;
    gap: 6px;
    padding: 16px;
    text-align: center;
    color: var(--text-2);
  }
  .state p {
    margin: 0;
  }
  .notice {
    margin: 0;
    padding: 6px 10px;
    font-size: 12px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }
  .notice.warn {
    background: var(--warn-soft, var(--surface-2));
  }
  .plist {
    flex: 1;
    min-height: 0;
    overflow: auto;
    margin: 0;
    padding: 6px 10px 6px 34px;
    font-size: 12px;
  }
  .plist li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 0;
  }
  .plist li.locked .desc {
    font-family: var(--font-mono);
    color: var(--text-2);
  }
  .plist .desc {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .strip {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
    padding: 6px 10px;
    border-top: 1px solid var(--border);
    font-size: 12px;
  }
  .lbl {
    font-weight: 600;
    margin-right: 2px;
  }
  .chip {
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .chip.locked {
    border-style: dashed;
    color: var(--text-2);
  }
  .small {
    font-size: 12px;
  }
  .phone .phead :global(.btn),
  .phone .plist :global(.btn) {
    min-height: 44px;
    min-width: 44px;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
