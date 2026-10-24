<script lang="ts">
  // One member of an entry of a structured settings field, such as a model's context
  // tokens or a role entry's provider: its label with the unit, a control that follows the
  // member's type and the member's error under it. An empty input unsets the member. Text
  // that does not read as a number is kept, so the check can say what is wrong with it.
  import { inputValue, type FieldDef } from '$lib/settings';

  let {
    def,
    value,
    id,
    disabled = false,
    error,
    placeholder,
    list,
    onchange,
  }: {
    def: FieldDef;
    value: unknown;
    id: string;
    disabled?: boolean;
    error?: string;
    /** What an unset member means here, such as the value it inherits. */
    placeholder?: string;
    /** The id of a datalist of suggestions. */
    list?: string;
    onchange: (v: unknown) => void;
  } = $props();

  const text = $derived(value === undefined || value === null ? '' : String(value));
  const errId = $derived(`${id}-error`);
  /** The unit after the label, unless the label names it already. */
  const unit = $derived(
    !def.unit || def.label.toLowerCase().includes(def.unit)
      ? null
      : def.unit === 'seconds'
        ? 's'
        : def.unit,
  );
  const shown = $derived(
    placeholder ?? def.placeholder ?? (def.fallback !== undefined ? String(def.fallback) : ''),
  );
</script>

<div class="entry-field" class:bad={!!error}>
  <label for={id} title={def.help ? `${def.label}: ${def.help}` : def.label}
    >{def.label}{#if unit}{' '}<span class="unit">({unit})</span>{/if}</label
  >
  {#if def.type === 'enum'}
    <select
      {id}
      class="select"
      value={text}
      {disabled}
      title={def.help}
      aria-invalid={error ? 'true' : undefined}
      aria-describedby={error ? errId : undefined}
      onchange={(e) => onchange(e.currentTarget.value === '' ? undefined : e.currentTarget.value)}
    >
      {#if def.optional || value === undefined}
        <option value="">{def.optional ? shown || 'not set' : 'choose'}</option>
      {/if}
      {#if text && !(def.options ?? []).includes(text)}<option value={text}>{text}</option>{/if}
      {#each def.options ?? [] as o (o)}<option value={o}>{o}</option>{/each}
    </select>
  {:else if def.type === 'bool'}
    <input
      {id}
      type="checkbox"
      checked={value === true}
      {disabled}
      onchange={(e) => onchange(e.currentTarget.checked)}
    />
  {:else}
    <input
      {id}
      class="input"
      class:mono={def.type === 'text' || def.type === 'secret'}
      value={text}
      placeholder={shown}
      {list}
      {disabled}
      title={def.help}
      autocomplete="off"
      spellcheck="false"
      inputmode={def.type === 'int' ? 'numeric' : def.type === 'number' ? 'decimal' : undefined}
      aria-invalid={error ? 'true' : undefined}
      aria-describedby={error ? errId : undefined}
      oninput={(e) => onchange(inputValue(def, e.currentTarget.value))}
    />
  {/if}
  {#if error}<p class="error" id={errId}>{error}</p>{/if}
</div>

<style>
  .entry-field {
    display: grid;
    gap: 3px;
    align-content: start;
    min-width: 0;
  }
  label {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: var(--fs-xs);
    font-weight: 500;
    color: var(--text-2);
  }
  .unit {
    font-weight: 400;
    color: var(--text-3);
  }
  .input,
  .select {
    width: 100%;
  }
  .bad .input,
  .bad .select {
    border-color: var(--danger);
  }
  .error {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--danger);
  }
</style>
