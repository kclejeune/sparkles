<script lang="ts">
  // A list of objects as a settings field, such as a role list of provider and model
  // pairs: a row per entry with a control per member, buttons to move it up or down and
  // to remove it, and a button that adds an entry. A member's error shows under it. An
  // unset list and an empty one mean different things, so each says what it means, and a
  // list with a fallback can go back to it. Members an entry has that the form does not
  // know stay as they are.
  import EntryField from './EntryField.svelte';
  import Icon from './Icon.svelte';
  import {
    getAt,
    parsePath,
    pathString,
    withAt,
    type FieldDef,
    type JsonObject,
  } from '$lib/settings';

  let {
    def,
    value,
    id,
    disabled = false,
    errors = {},
    suggest,
    onchange,
  }: {
    def: FieldDef;
    value: unknown;
    /** The id of the Add button, which the row's label names. */
    id: string;
    disabled?: boolean;
    /** Errors by `index.member`. */
    errors?: Record<string, string>;
    /** The models to suggest for an entry's provider. */
    suggest?: (entry: JsonObject) => readonly string[];
    onchange: (v: unknown) => void;
  } = $props();

  const entries = $derived(Array.isArray(value) ? (value as unknown[]) : null);
  const fields = $derived(def.entry ?? []);

  const asObject = (x: unknown): JsonObject =>
    x && typeof x === 'object' && !Array.isArray(x) ? (x as JsonObject) : {};

  function setMember(i: number, f: FieldDef, v: unknown) {
    if (!entries) return;
    const next = entries.map((x) => x);
    next[i] = withAt(asObject(next[i]), parsePath(f.path), v);
    onchange(next);
  }

  function move(i: number, by: number) {
    if (!entries) return;
    const next = [...entries];
    const [x] = next.splice(i, 1);
    next.splice(i + by, 0, x);
    onchange(next);
  }

  function addEntry() {
    const first: JsonObject = {};
    for (const f of fields)
      if (f.type === 'enum' && !f.optional && f.options?.length) first[f.path] = f.options[0];
    onchange([...(entries ?? []), first]);
  }

  const label = (x: unknown, i: number) => {
    const e = asObject(x);
    return e.provider && e.model ? `${e.provider}/${e.model}` : `entry ${i + 1}`;
  };
  const err = (i: number, f: FieldDef) => errors[pathString([String(i), ...parsePath(f.path)])];
</script>

<div class="records">
  {#if entries == null}
    <p class="state">Not set: {def.unsetText ? `uses ${def.unsetText}` : 'the role is off'}.</p>
  {:else if entries.length === 0}
    <p class="state">
      {def.emptyText ?? 'The list is empty.'}
      {#if def.unsetText}
        <button type="button" class="btn ghost sm" {disabled} onclick={() => onchange(undefined)}
          >Use {def.unsetText}</button
        >
      {/if}
    </p>
  {:else}
    <ol class="entries" aria-label={def.label}>
      {#each entries as x, i (i)}
        {@const entry = asObject(x)}
        {@const options = suggest?.(entry) ?? []}
        <li class="entry" aria-label="{def.label} {label(x, i)}">
          <span class="n">{i + 1}</span>
          <div class="members">
            {#each fields as f (f.path)}
              {@const fid = `${id}-${i}-${f.path.replace(/[^A-Za-z0-9]+/g, '-')}`}
              <EntryField
                def={f}
                value={getAt(entry, parsePath(f.path))}
                id={fid}
                {disabled}
                error={err(i, f)}
                list={f.path === 'model' && options.length ? `${fid}-options` : undefined}
                onchange={(v) => setMember(i, f, v)}
              />
              {#if f.path === 'model' && options.length}
                <datalist id="{fid}-options">
                  {#each options as m (m)}<option value={m}></option>{/each}
                </datalist>
              {/if}
            {/each}
          </div>
          <div class="tools">
            <button
              type="button"
              class="btn ghost icon sm"
              disabled={disabled || i === 0}
              aria-label="Move {label(x, i)} up"
              onclick={() => move(i, -1)}><Icon name="arrowUp" size={13} /></button
            >
            <button
              type="button"
              class="btn ghost icon sm"
              disabled={disabled || i === entries.length - 1}
              aria-label="Move {label(x, i)} down"
              onclick={() => move(i, 1)}><Icon name="arrowDown" size={13} /></button
            >
            <button
              type="button"
              class="btn ghost icon sm"
              {disabled}
              aria-label="Remove {label(x, i)}"
              onclick={() => onchange(entries.filter((_, j) => j !== i))}
              ><Icon name="x" size={13} /></button
            >
          </div>
        </li>
      {/each}
    </ol>
  {/if}
  {#if errors['']}<p class="error">{errors['']}</p>{/if}
  {#if !disabled}
    <div>
      <button
        {id}
        type="button"
        class="btn sm"
        aria-label="Add {def.keyLabel ?? 'entry'} to {def.label}"
        onclick={addEntry}><Icon name="plus" size={12} /> Add {def.keyLabel ?? 'entry'}</button
      >
    </div>
  {/if}
</div>

<style>
  /* the editor fits the width of its row, however long its entries */
  .records > :global(*) {
    min-width: 0;
  }
  .records {
    display: grid;
    gap: 6px;
    min-width: 0;
    max-width: 760px;
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
  .entries {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  .entry {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    gap: 8px;
    align-items: start;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .n {
    padding-top: 22px;
    text-align: right;
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .members {
    display: grid;
    grid-template-columns: minmax(110px, 1fr) minmax(140px, 1.6fr) minmax(90px, 0.8fr) minmax(
        90px,
        0.8fr
      );
    gap: 6px 8px;
    min-width: 0;
  }
  .tools {
    display: flex;
    gap: 2px;
    padding-top: 18px;
  }
  .error {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--danger);
  }
  @media (max-width: 760px) {
    .members {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
    .entry {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .n {
      display: none;
    }
    .tools {
      flex-direction: column;
      padding-top: 16px;
    }
  }
</style>
