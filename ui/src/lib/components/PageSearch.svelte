<script lang="ts">
  // A page's own search field (ROADMAP item 633): one tall field across the content width,
  // outside any panel, as Browse's landing page and Memory use it. Enter calls `onsubmit`,
  // and the caller decides whether typing searches at once.
  import Icon from './Icon.svelte';

  let {
    value = $bindable(''),
    placeholder,
    label,
    busy = false,
    onsubmit,
    field = $bindable(),
  }: {
    value?: string;
    placeholder: string;
    /** The field's accessible name. */
    label: string;
    /** Shows a spinner in place of the search icon. */
    busy?: boolean;
    onsubmit?: () => void;
    field?: HTMLInputElement;
  } = $props();
</script>

<form
  class="page-search"
  role="search"
  aria-label={label}
  onsubmit={(e) => {
    e.preventDefault();
    onsubmit?.();
  }}
>
  <label class="field">
    {#if busy}<span class="spinner"></span>{:else}<Icon name="search" size={18} />{/if}
    <input
      type="search"
      bind:this={field}
      bind:value
      {placeholder}
      aria-label={label}
      autocomplete="off"
      spellcheck="false"
    />
  </label>
</form>

<style>
  .page-search {
    min-width: 0;
  }
  .field {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 44px;
    padding: 0 var(--sp-4);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-lg);
    background: var(--surface);
    color: var(--text-3);
    min-width: 0;
  }
  .field:focus-within {
    box-shadow: var(--focus);
  }
  input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    outline: none;
    font-size: 15px;
    color: var(--text);
    height: 100%;
    padding: 0;
  }
</style>
