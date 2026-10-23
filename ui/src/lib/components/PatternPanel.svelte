<script lang="ts">
  // The panel of one element of the Pattern pane (spec U08 §2.2, §2.5): what the element
  // is, where it is in the text, and every canvas edit on it as a control. An edge changes
  // its predicate or path, turns optional on or off, or goes. A variable is renamed, gets
  // filters, starts an edge or goes with its triples. A locked block shows its text and
  // goes only after a confirmation the pane asks for.
  import type { Drawing, PatternOk } from '$lib/pattern';
  import { describe } from '$lib/pattern';
  import { tick } from 'svelte';
  import { filterKeys, lockedBlocks, pathForms, type AnyEdit } from '$lib/pattern-edits';
  import { expression, formOf, formsFor, NO_VALUE, OP_LABEL, type Op } from '$lib/pattern-filters';
  import type { Choice } from '$lib/pattern-schema';
  import type { Focusable } from './PatternCanvas.svelte';
  import Icon from './Icon.svelte';
  import PatternPicker from './PatternPicker.svelte';

  let {
    el,
    drawing,
    answer,
    text,
    readonly = false,
    sheet = false,
    intent = null,
    predicates,
    short,
    note = null,
    datatypes,
    onedit,
    onrename,
    onedge,
    ondeletelocked,
    onremovewaiting,
    onclose,
  }: {
    el: Focusable;
    drawing: Drawing;
    answer: PatternOk;
    /** the query text, for line numbers */
    text: string;
    /** no edits: a result's pattern, or text that does not parse */
    readonly?: boolean;
    /** a bottom sheet on a phone */
    sheet?: boolean;
    /** what opens with the panel: the filter form, after `F` */
    intent?: 'filter' | null;
    /** the predicate picker's choices for this element */
    predicates: (typed: string) => Choice[];
    short: (iri: string) => string;
    /** why the picker has no schema, when it has none */
    note?: string | null;
    /** the datatypes a variable's values were seen with, or null */
    datatypes: (v: string) => string[] | null;
    onedit: (e: AnyEdit) => void;
    /** a rename, which the pane confirms when locked blocks use the variable */
    onrename: (from: string, to: string) => void;
    /** start an edge from this node */
    onedge: (node: string) => void;
    /** delete a locked block or chip, after the pane's confirmation */
    ondeletelocked: (key: string, text: string, filter: boolean) => void;
    onremovewaiting: (node: string) => void;
    onclose: () => void;
  } = $props();

  const lineOf = (offset: number) => text.slice(0, offset).split('\n').length;
  const edge = $derived('edge' in el ? el.edge : undefined);
  const triple = $derived(edge?.triple);
  const node = $derived('node' in el ? el.node : undefined);
  const locked = $derived(node?.locked);
  const isVar = $derived(node?.kind === 'var' && !node.fromLocked);
  const varName = $derived(isVar && node ? node.id.slice(2) : '');

  // the predicate picker of an edge
  let newPred = $state('');
  // the rename field of a variable
  let newName = $state('');
  // the filter form of a variable: a new filter, or a chip being edited
  let filterOpen = $state(false);
  let filterKey = $state<string | null>(null);
  let filterOp = $state<Op>('=');
  let filterValue = $state('');
  /** a regex's flags: `i` for a new filter, those of the chip for one being edited */
  let filterFlags = $state('i');
  $effect(() => {
    // a new element starts with empty fields
    void el;
    newPred = '';
    newName = '';
    filterOpen = false;
    filterKey = null;
    if (intent === 'filter' && isVar) {
      openFilter(null);
      void tick().then(() => panel?.querySelector<HTMLElement>('select')?.focus());
    }
  });
  let panel: HTMLElement | undefined = $state();
  /** whether a locked node is a block of the WHERE group, which the canvas may delete */
  const deletable = $derived(
    !!node && (node.kind === 'chip' || lockedBlocks(answer).some((b) => b.key === node.id)),
  );

  const dts = $derived(isVar ? datatypes(varName) : null);
  const ops = $derived(formsFor(dts));
  /** The filters on this variable that the canvas understands, with their keys. */
  const chips = $derived(
    node?.filters && node.kind !== 'chip'
      ? node.filters.map((key, i) => ({
          key,
          label: node.subtitle.slice(-node.filters!.length)[i],
        }))
      : [],
  );

  function openFilter(key: string | null) {
    filterKey = key;
    filterOpen = true;
    filterOp = ops[0] ?? '=';
    filterValue = '';
    filterFlags = 'i';
    if (key) {
      const f = filterOf(key);
      if (f?.expr) {
        const form = formOf(f.expr);
        filterOp = form.op;
        filterValue = form.value;
        filterFlags = form.flags ?? 'i';
      }
    }
  }

  /** The filter of a key, among the pattern's filters. */
  const filterOf = (key: string) => filterKeys(answer).find((x) => x.key === key)?.filter;

  function submitFilter(ev: SubmitEvent) {
    ev.preventDefault();
    const expr = expression(filterOp, varName, filterValue, dts?.[0] ?? null, filterFlags);
    onedit(
      filterKey
        ? { kind: 'filter-edit', filter: filterKey, expr }
        : { kind: 'filter-add', var: varName, expr },
    );
  }
</script>

<div
  class="panel"
  bind:this={panel}
  class:sheet
  role="dialog"
  aria-label="Pattern element"
  aria-modal="false"
>
  <div class="head">
    <strong>{describe(el, drawing)}</strong>
    <button class="btn ghost icon sm" aria-label="Close" title="Close (Escape)" onclick={onclose}
      ><Icon name="x" size={12} /></button
    >
  </div>
  {#if edge && triple}
    <p class="faint small">
      Line {lineOf(triple.triple.range.from)}:
      <span class="mono">{triple.triple.s.text} {triple.triple.p.text} {triple.triple.o.text}</span>
    </p>
    <div class="actions">
      <button
        class="btn sm"
        disabled={readonly}
        onclick={() => onedit({ kind: 'optional', edge: edge.id })}
        aria-pressed={triple.path.length > 1}
      >
        {triple.path.length > 1 ? 'Required' : 'Optional'}
      </button>
      <button
        class="btn sm danger"
        disabled={readonly}
        onclick={() => onedit({ kind: 'delete', edge: edge.id })}
      >
        <Icon name="trash" size={13} /> Delete
      </button>
    </div>
    {#if !readonly}
      <form
        class="row"
        aria-label="Change the predicate"
        onsubmit={(ev) => {
          ev.preventDefault();
          if (newPred.trim()) onedit({ kind: 'predicate', edge: edge.id, p: newPred });
        }}
      >
        <PatternPicker
          label="New predicate"
          name="pattern-predicate"
          bind:value={newPred}
          placeholder={triple.triple.p.text}
          choices={predicates}
          {short}
          {note}
        />
        <button class="btn sm" type="submit">Change</button>
      </form>
      {#if triple.triple.p.kind !== 'var'}
        <div class="actions" role="group" aria-label="Path">
          {#each pathForms(triple.triple.p.text) as f (f.text)}
            <button
              class="btn sm"
              aria-pressed={f.text === triple.triple.p.text}
              title={f.text}
              onclick={() =>
                f.text !== triple.triple.p.text &&
                onedit({ kind: 'predicate', edge: edge.id, p: f.text })}>{f.label}</button
            >
          {/each}
        </div>
      {/if}
    {/if}
  {:else if locked}
    <p class="faint small">
      Line {lineOf(locked.range.from)}. Locked parts are edited in the editor, and canvas edits
      leave them as they are.
    </p>
    <pre class="mono locked-text">{locked.text}</pre>
    {#if node?.warn}
      <p class="warn small" role="note">{node.subtitle.at(-1)}</p>
    {/if}
    {#if !readonly && node && deletable}
      <div class="actions">
        <button
          class="btn sm danger"
          onclick={() =>
            ondeletelocked(
              node.kind === 'chip' ? node.filters![0] : node.id,
              locked.text,
              node.kind === 'chip',
            )}><Icon name="trash" size={13} /> Delete…</button
        >
      </div>
    {/if}
  {:else if node}
    <p class="faint small">{node.title}</p>
    {#if node.waiting}
      <p class="small">This node waits for an edge, since SPARQL has no free variables.</p>
    {/if}
    {#if !readonly}
      <div class="actions">
        {#if node.kind !== 'literal'}
          <button class="btn sm" onclick={() => onedge(node.id)}>Edge from here</button>
        {/if}
        {#if isVar && !node.waiting}
          <button class="btn sm" onclick={() => openFilter(null)}>Filter</button>
        {/if}
        {#if node.waiting}
          <button class="btn sm danger" onclick={() => onremovewaiting(node.id)}>Remove</button>
        {:else}
          <button
            class="btn sm danger"
            onclick={() => onedit({ kind: 'delete-node', node: node.id })}
          >
            <Icon name="trash" size={13} /> Delete
          </button>
        {/if}
      </div>
      {#if chips.length}
        <ul class="chips" aria-label="Filters on {node.label}">
          {#each chips as c (c.key)}
            <li>
              <span class="mono">{c.label}</span>
              <button class="btn sm ghost" onclick={() => openFilter(c.key)}>Edit</button>
              <button
                class="btn sm ghost"
                aria-label="Delete the filter {c.label}"
                onclick={() => onedit({ kind: 'filter-delete', filter: c.key })}
                ><Icon name="trash" size={12} /></button
              >
            </li>
          {/each}
        </ul>
      {/if}
      {#if filterOpen}
        <form
          class="row"
          aria-label={filterKey ? 'Edit the filter' : 'Add a filter'}
          onsubmit={submitFilter}
        >
          <select aria-label="Filter form" bind:value={filterOp}>
            {#each ops as op (op)}<option value={op}>{OP_LABEL[op]}</option>{/each}
          </select>
          {#if !NO_VALUE.has(filterOp)}
            <input aria-label="Filter value" bind:value={filterValue} required />
          {/if}
          <button class="btn sm primary" type="submit">{filterKey ? 'Change' : 'Add'}</button>
        </form>
      {/if}
      {#if isVar && !node.waiting}
        <form
          class="row"
          aria-label="Rename the variable"
          onsubmit={(ev) => {
            ev.preventDefault();
            if (newName.trim()) onrename(varName, newName);
          }}
        >
          <input aria-label="New name" placeholder="?{varName}" bind:value={newName} />
          <button class="btn sm" type="submit">Rename</button>
        </form>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .panel {
    position: absolute;
    right: 10px;
    bottom: 10px;
    width: min(360px, calc(100% - 20px));
    max-height: calc(100% - 20px);
    overflow: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-pop);
    padding: 10px 12px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .panel.sheet {
    left: 0;
    right: 0;
    bottom: 0;
    width: auto;
    border-radius: 12px 12px 0 0;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    justify-content: space-between;
    font-size: var(--fs-sm);
  }
  .actions,
  .row {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .row input,
  .row select {
    flex: 1 1 100px;
    min-width: 0;
    font-size: 12px;
  }
  .sheet .actions :global(.btn),
  .sheet .row :global(.btn),
  .sheet .row input,
  .sheet .row select {
    min-height: 44px;
  }
  .sheet .actions :global(.btn) {
    flex: 1;
  }
  .chips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
  }
  .chips li {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .chips .mono {
    flex: 1;
  }
  .small {
    font-size: var(--fs-xs, 12px);
    margin: 0;
  }
  .warn {
    color: var(--danger);
  }
  .locked-text {
    margin: 0;
    max-height: 160px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 11px;
    background: var(--surface-2);
    padding: 6px 8px;
    border-radius: 4px;
  }
</style>
