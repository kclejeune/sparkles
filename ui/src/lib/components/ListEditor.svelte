<script lang="ts">
  // A list of strings as a settings field, such as a provider's allowed models: one row
  // per entry with buttons to move it up or down and to remove it, an input that adds an
  // entry and suggests known values, and the suggestions not in the list as buttons. An
  // unset list and an empty one mean different things, so each says what it means.
  import Icon from './Icon.svelte';
  import type { FieldDef } from '$lib/settings';

  let {
    def,
    value,
    id,
    disabled = false,
    errors = {},
    suggestions = [],
    onchange,
  }: {
    def: FieldDef;
    value: unknown;
    /** The id of the input that adds an entry, which the row's label names. */
    id: string;
    disabled?: boolean;
    /** Errors by entry index. */
    errors?: Record<string, string>;
    suggestions?: readonly string[];
    onchange: (v: unknown) => void;
  } = $props();

  const items = $derived(Array.isArray(value) ? value.map((x) => String(x ?? '')) : null);
  const missing = $derived(suggestions.filter((s) => !(items ?? []).includes(s)).slice(0, 8));
  let draft = $state('');
  let addError = $state<string | null>(null);

  function add(text: string) {
    const t = text.trim();
    if (!t) return;
    if ((items ?? []).includes(t)) {
      addError = `${t} is in the list already`;
      return;
    }
    if (def.pattern && !new RegExp(def.pattern, 'u').test(t)) {
      addError = def.patternText ?? `${t} does not have the right form`;
      return;
    }
    addError = null;
    onchange([...(items ?? []), t]);
    draft = '';
  }

  function move(i: number, by: number) {
    if (!items) return;
    const next = [...items];
    const [x] = next.splice(i, 1);
    next.splice(i + by, 0, x);
    onchange(next);
  }

  const what = (x: string, i: number) => x || `entry ${i + 1}`;
</script>

<div class="list-editor">
  {#if items == null}
    <p class="state">Not set{def.placeholder ? `: ${def.placeholder}` : ''}.</p>
  {:else if items.length === 0}
    <p class="state">
      {def.emptyText ?? 'The list is empty.'}
      {#if def.optional}
        <button type="button" class="btn ghost sm" {disabled} onclick={() => onchange(undefined)}
          >Unset{def.placeholder ? `: ${def.placeholder}` : ''}</button
        >
      {/if}
    </p>
  {:else}
    <ol class="items" aria-label={def.label}>
      {#each items as item, i (i)}
        <li class:bad={!!errors[String(i)]}>
          <div class="row">
            <span class="n">{i + 1}</span>
            <input
              class="input mono"
              value={item}
              {disabled}
              aria-label="{def.label} entry {i + 1}"
              aria-invalid={errors[String(i)] ? 'true' : undefined}
              oninput={(e) => {
                const next = [...items];
                next[i] = e.currentTarget.value;
                onchange(next);
              }}
            />
            <button
              type="button"
              class="btn ghost icon sm"
              disabled={disabled || i === 0}
              aria-label="Move {what(item, i)} up"
              onclick={() => move(i, -1)}><Icon name="arrowUp" size={13} /></button
            >
            <button
              type="button"
              class="btn ghost icon sm"
              disabled={disabled || i === items.length - 1}
              aria-label="Move {what(item, i)} down"
              onclick={() => move(i, 1)}><Icon name="arrowDown" size={13} /></button
            >
            <button
              type="button"
              class="btn ghost icon sm"
              {disabled}
              aria-label="Remove {what(item, i)}"
              onclick={() => onchange(items.filter((_, j) => j !== i))}
              ><Icon name="x" size={13} /></button
            >
          </div>
          {#if errors[String(i)]}<p class="error">{errors[String(i)]}</p>{/if}
        </li>
      {/each}
    </ol>
  {/if}
  {#if !disabled}
    <div class="add">
      <input
        {id}
        class="input mono"
        list="{id}-options"
        placeholder="Add a {def.keyLabel ?? 'model'}"
        autocomplete="off"
        spellcheck="false"
        bind:value={draft}
        aria-invalid={addError ? 'true' : undefined}
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            add(draft);
          }
        }}
      />
      <button type="button" class="btn sm" disabled={!draft.trim()} onclick={() => add(draft)}
        ><Icon name="plus" size={12} /> Add</button
      >
      <datalist id="{id}-options">
        {#each missing as s (s)}<option value={s}></option>{/each}
      </datalist>
    </div>
    {#if addError}<p class="error">{addError}</p>{/if}
    {#if missing.length}
      <div class="suggest" aria-label="Suggested">
        <span class="faint">Known:</span>
        {#each missing as s (s)}
          <button type="button" class="chip mono" aria-label="Add {s}" onclick={() => add(s)}
            ><Icon name="plus" size={11} /> {s}</button
          >
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  /* the editor fits the width of its row, however long its entries */
  .list-editor > :global(*) {
    min-width: 0;
  }
  .list-editor {
    display: grid;
    gap: 6px;
    max-width: 640px;
    min-width: 0;
  }
  .state {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .items {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .row,
  .add {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .row .input,
  .add .input {
    flex: 1 1 0;
    width: 0;
    min-width: 0;
  }
  .n {
    width: 18px;
    text-align: right;
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .bad .input,
  .add .input[aria-invalid='true'] {
    border-color: var(--danger);
  }
  .error {
    margin: 0 0 0 22px;
    font-size: var(--fs-xs);
    color: var(--danger);
  }
  .add + .error {
    margin-left: 0;
  }
  .suggest {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-xs);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 7px;
    border: 1px dashed var(--border-strong);
    border-radius: 999px;
    background: transparent;
    color: var(--text-2);
    font-size: var(--fs-xs);
    cursor: pointer;
  }
  .chip:hover,
  .chip:focus-visible {
    border-style: solid;
    color: var(--text);
    background: var(--surface-2);
  }
</style>
