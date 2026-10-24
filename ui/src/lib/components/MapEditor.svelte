<script lang="ts">
  // An object of named entries as a settings field, such as a provider's per-model
  // options: a card per name that opens to a control per option, with each option's
  // error under it and the value it inherits as its placeholder. The summary of a closed
  // card lists the options it sets. An input adds a name and suggests known ones. A name
  // that the server's configuration declares cannot be removed here, since the runtime
  // layer cannot remove it, and options the form does not know are listed and kept.
  import EntryField from './EntryField.svelte';
  import Icon from './Icon.svelte';
  import {
    getAt,
    parsePath,
    pathString,
    inheritedFor,
    valueText,
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
    suggestions = [],
    inherited = {},
    declared = [],
    onchange,
  }: {
    def: FieldDef;
    value: unknown;
    /** The id of the input that adds an entry, which the row's label names. */
    id: string;
    disabled?: boolean;
    /** Errors by `name.member`, or by `name` for the entry itself. */
    errors?: Record<string, string>;
    suggestions?: readonly string[];
    /** The value each member inherits when an entry leaves it out, by member path. */
    inherited?: Record<string, unknown>;
    /** The names the server's configuration declares. */
    declared?: readonly string[];
    onchange: (v: unknown) => void;
  } = $props();

  const asObject = (x: unknown): JsonObject =>
    x && typeof x === 'object' && !Array.isArray(x) ? (x as JsonObject) : {};
  const entries = $derived(Object.entries(asObject(value)));
  const fields = $derived(def.entry ?? []);
  const known = $derived(new Set(fields.map((f) => parsePath(f.path)[0])));
  const missing = $derived(suggestions.filter((s) => !(s in asObject(value))));
  let draft = $state('');
  let addError = $state<string | null>(null);
  /** The names whose card is open. */
  let open = $state<Record<string, boolean>>({});

  const errorsOf = (name: string) =>
    Object.keys(errors).filter(
      (k) => k === pathString([name]) || k.startsWith(`${pathString([name])}.`),
    );
  const err = (name: string, f: FieldDef) => errors[pathString([name, ...parsePath(f.path)])];

  function setMember(name: string, f: FieldDef, v: unknown) {
    const next: JsonObject = {};
    for (const [n, x] of entries)
      next[n] = n === name ? withAt(asObject(x), parsePath(f.path), v) : x;
    onchange(next);
  }

  function add(text: string) {
    const t = text.trim();
    if (!t) return;
    if (t in asObject(value)) {
      addError = `${t} has options already`;
      open = { ...open, [t]: true };
      return;
    }
    if (def.keyPattern && !new RegExp(def.keyPattern, 'u').test(t)) {
      addError = `${t} is not a ${def.keyLabel ?? 'name'} that the server accepts`;
      return;
    }
    addError = null;
    // an entry without options would vanish from the runtime layer, so a new one states
    // the default structured output level, as the Add provider form does
    const first: JsonObject = fields.some((f) => f.path === 'structuredOutput')
      ? { structuredOutput: 'auto' }
      : {};
    onchange({ ...asObject(value), [t]: first });
    open = { ...open, [t]: true };
    draft = '';
  }

  function remove(name: string) {
    const next = { ...asObject(value) };
    delete next[name];
    onchange(next);
  }

  /** The options an entry sets, in a line. */
  function summary(x: unknown): string {
    const e = asObject(x);
    const parts = fields
      .filter((f) => getAt(e, parsePath(f.path)) !== undefined)
      .map((f) => {
        const v = getAt(e, parsePath(f.path));
        return `${f.label} ${typeof v === 'string' ? v : valueText(v, 24)}`;
      });
    const other = Object.keys(e).filter((k) => !known.has(k));
    if (other.length) parts.push(`${other.length} other`);
    return parts.length ? parts.join(' · ') : 'no options';
  }

  const placeholderOf = (f: FieldDef, entry: unknown) => {
    const own = inheritedFor(fields, entry, inherited);
    const v = own[f.path];
    if (v === undefined) return undefined;
    return inherited[f.path] === v && v !== f.fallback
      ? `${valueText(v, 24)} (provider)`
      : valueText(v, 24);
  };
</script>

<div class="map-editor">
  {#if entries.length === 0}
    <p class="state">No {def.keyLabel ?? 'entry'} has options of its own.</p>
  {:else}
    <ul class="cards" aria-label={def.label}>
      {#each entries as [name, x] (name)}
        {@const bad = errorsOf(name).length}
        {@const isDeclared = declared.includes(name)}
        <li class="card" class:bad={bad > 0} data-entry={name}>
          <details
            open={open[name] || bad > 0}
            ontoggle={(e) => (open = { ...open, [name]: e.currentTarget.open })}
          >
            <summary>
              <Icon name="chevron" size={12} class="chev" />
              <span class="mono name">{name}</span>
              <span class="faint sum">{summary(x)}</span>
              {#if bad}<span class="badge danger">{bad} error{bad === 1 ? '' : 's'}</span>{/if}
            </summary>
            <div class="body">
              {#if errors[pathString([name])]}<p class="error">{errors[pathString([name])]}</p>{/if}
              <div class="members">
                {#each fields as f (f.path)}
                  <EntryField
                    def={f}
                    value={getAt(asObject(x), parsePath(f.path))}
                    id="{id}-{name.replace(/[^A-Za-z0-9]+/g, '-')}-{f.path.replace(
                      /[^A-Za-z0-9]+/g,
                      '-',
                    )}"
                    {disabled}
                    error={err(name, f)}
                    placeholder={placeholderOf(f, x)}
                    onchange={(v) => setMember(name, f, v)}
                  />
                {/each}
              </div>
              {#if Object.keys(asObject(x)).some((k) => !known.has(k))}
                <p class="faint small">
                  Other options, kept as they are and edited under Advanced:
                  {#each Object.keys(asObject(x)).filter((k) => !known.has(k)) as k, i (k)}{i
                      ? ', '
                      : ''}<code>{k}</code>{/each}.
                </p>
              {/if}
              <div class="card-tools">
                <span class="spacer"></span>
                <button
                  type="button"
                  class="btn danger sm"
                  disabled={disabled || isDeclared}
                  title={isDeclared
                    ? "The server's model configuration declares this entry, so it cannot be removed here. Reset its options instead."
                    : undefined}
                  aria-label="Remove the options of {name}"
                  onclick={() => remove(name)}><Icon name="trash" size={12} /> Remove</button
                >
              </div>
            </div>
          </details>
        </li>
      {/each}
    </ul>
  {/if}
  {#if errors['']}<p class="error">{errors['']}</p>{/if}
  {#if !disabled}
    <div class="add">
      <input
        {id}
        class="input mono"
        list="{id}-options"
        placeholder="Add options for a {def.keyLabel ?? 'name'}"
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
        ><Icon name="plus" size={12} /> Add {def.keyLabel ?? 'entry'}</button
      >
      <datalist id="{id}-options">
        {#each missing as s (s)}<option value={s}></option>{/each}
      </datalist>
    </div>
    {#if addError}<p class="error">{addError}</p>{/if}
  {/if}
</div>

<style>
  /* the editor fits the width of its row, however long its entries */
  .map-editor > :global(*) {
    min-width: 0;
  }
  .map-editor {
    display: grid;
    gap: 6px;
    min-width: 0;
    max-width: 760px;
  }
  .state {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
    min-width: 0;
  }
  .card.bad {
    border-color: color-mix(in srgb, var(--danger) 55%, transparent);
  }
  summary {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    padding: 6px 8px;
    cursor: pointer;
    list-style: none;
    min-width: 0;
  }
  summary::-webkit-details-marker {
    display: none;
  }
  summary :global(.chev) {
    color: var(--text-3);
    transition: transform 0.12s;
  }
  details[open] > summary :global(.chev) {
    transform: rotate(90deg);
  }
  .name {
    font-weight: 600;
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  .sum {
    font-size: var(--fs-xs);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .body {
    display: grid;
    gap: 8px;
    padding: 2px 10px 8px 28px;
  }
  .members {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 8px 10px;
  }
  .card-tools {
    display: flex;
  }
  .add {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .add .input {
    flex: 1 1 0;
    width: 0;
    min-width: 0;
  }
  .add .input[aria-invalid='true'] {
    border-color: var(--danger);
  }
  .error {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--danger);
  }
  .small {
    margin: 0;
    font-size: var(--fs-xs);
  }
  @media (max-width: 760px) {
    .body {
      padding-left: 10px;
    }
    .members {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
  }
</style>
