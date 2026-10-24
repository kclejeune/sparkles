<script lang="ts">
  // An object of strings as a settings field, such as a provider's extra headers: a row
  // of name and value per member, a button to remove it, and a row that adds one. A
  // renamed member keeps its place.
  import Icon from './Icon.svelte';
  import { pathString, type FieldDef, type JsonObject } from '$lib/settings';

  let {
    def,
    value,
    id,
    disabled = false,
    errors = {},
    onchange,
  }: {
    def: FieldDef;
    value: unknown;
    /** The id of the name input of the Add row, which the row's label names. */
    id: string;
    disabled?: boolean;
    /** Errors by member name. */
    errors?: Record<string, string>;
    onchange: (v: unknown) => void;
  } = $props();

  const pairs = $derived(
    Object.entries(
      value && typeof value === 'object' && !Array.isArray(value) ? (value as JsonObject) : {},
    ),
  );
  let name = $state('');
  let text = $state('');
  let addError = $state<string | null>(null);
  const what = $derived(def.keyLabel ?? 'name');

  /** Why a rename was refused, by the name the row keeps. */
  let renameError = $state<Record<string, string>>({});
  // any change of the value, from this editor or from outside, starts without them
  $effect(() => {
    void value;
    renameError = {};
  });

  /** Rename row `i`, or refuse and put its name back in `input`. */
  function rename(i: number, to: string, input: HTMLInputElement) {
    const from = pairs[i][0];
    if (to === from) return;
    const clash = pairs.find(([n], j) => j !== i && n.toLowerCase() === to.toLowerCase());
    if (!to || clash) {
      input.value = from;
      renameError = {
        ...renameError,
        [from]: to
          ? `${clash![0]} is set already, so ${from} keeps its name`
          : `A ${what} needs a name, so ${from} keeps its name`,
      };
      return;
    }
    const next: JsonObject = {};
    pairs.forEach(([n, v], j) => (next[j === i ? to : n] = v));
    onchange(next);
  }

  function setValue(i: number, v: string) {
    const next: JsonObject = {};
    pairs.forEach(([n, x], j) => (next[n] = j === i ? v : x));
    onchange(next);
  }

  function add() {
    const n = name.trim();
    if (!n) return;
    if (pairs.some(([x]) => x.toLowerCase() === n.toLowerCase())) {
      addError = `${n} is set already`;
      return;
    }
    addError = null;
    onchange({ ...Object.fromEntries(pairs), [n]: text });
    name = text = '';
  }
</script>

<div class="pairs">
  {#if pairs.length === 0}
    <p class="state">None.</p>
  {:else}
    <ul class="rows" aria-label={def.label}>
      {#each pairs as [n, v], i (i)}
        {@const e = renameError[n] ?? errors[pathString([n])]}
        <li>
          <div class="row">
            <input
              class="input mono"
              value={n}
              {disabled}
              aria-label="{def.label} {what} {i + 1}"
              aria-invalid={e ? 'true' : undefined}
              onchange={(ev) => rename(i, ev.currentTarget.value.trim(), ev.currentTarget)}
            />
            <input
              class="input mono"
              value={String(v ?? '')}
              {disabled}
              aria-label="Value of {n}"
              oninput={(ev) => setValue(i, ev.currentTarget.value)}
            />
            <button
              type="button"
              class="btn ghost icon sm"
              {disabled}
              aria-label="Remove {n}"
              onclick={() => onchange(Object.fromEntries(pairs.filter((_, j) => j !== i)))}
              ><Icon name="x" size={13} /></button
            >
          </div>
          {#if e}<p class="error">{e}</p>{/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if !disabled}
    <div class="row">
      <input
        {id}
        class="input mono"
        placeholder="New {what}"
        autocomplete="off"
        spellcheck="false"
        bind:value={name}
      />
      <input
        class="input mono"
        placeholder="Value"
        aria-label="Value of the new {what}"
        autocomplete="off"
        spellcheck="false"
        bind:value={text}
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            add();
          }
        }}
      />
      <button type="button" class="btn sm" disabled={!name.trim()} onclick={add}
        ><Icon name="plus" size={12} /> Add</button
      >
    </div>
    {#if addError}<p class="error">{addError}</p>{/if}
  {/if}
</div>

<style>
  /* the editor fits the width of its row, however long its entries */
  .pairs > :global(*) {
    min-width: 0;
  }
  .pairs {
    display: grid;
    gap: 4px;
    max-width: 640px;
    min-width: 0;
  }
  .state {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr) auto;
    gap: 4px;
    align-items: center;
  }
  .input[aria-invalid='true'] {
    border-color: var(--danger);
  }
  .error {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--danger);
  }
</style>
