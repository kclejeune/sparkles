<script lang="ts">
  // The sources of a definition or synonym in edit mode: each a chip with a remove button,
  // and a field that adds one on Enter or on leaving it, such as `GOC:al` or `PMID:123`.
  // The field's text is bound out, so a preview by keyboard includes a source not added yet.
  import Icon from '$components/Icon.svelte';

  let {
    sources = $bindable([]),
    pending = $bindable(''),
    label,
    disabled = false,
  }: {
    sources?: string[];
    /** The text typed in the add field and not added, which a preview counts as a source. */
    pending?: string;
    /** What the sources belong to, for the accessible names. */
    label: string;
    disabled?: boolean;
  } = $props();

  function add() {
    const s = pending.trim();
    pending = '';
    if (s && !sources.includes(s)) sources = [...sources, s];
  }
</script>

<span class="chips" role="group" aria-label="Sources of {label}">
  {#each sources as s (s)}
    <span class="chipx"
      >{s}<button
        type="button"
        class="x"
        aria-label="Remove the source {s}"
        {disabled}
        onclick={() => (sources = sources.filter((x) => x !== s))}
        ><Icon name="x" size={12} /></button
      ></span
    >
  {/each}
  <input
    class="input sm add"
    bind:value={pending}
    placeholder="Add source"
    aria-label="Add a source to {label}"
    size="12"
    {disabled}
    onkeydown={(e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        add();
      }
    }}
    onblur={add}
  />
</span>

<style>
  .chips {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .chipx {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: var(--ctl-h-sm);
    padding: 0 2px 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--r-pill);
    font-size: var(--fs-sm);
    color: var(--text-2);
    background: var(--surface);
    overflow-wrap: anywhere;
  }
  .x {
    display: inline-grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border: 0;
    border-radius: 50%;
    background: none;
    color: var(--text-3);
    cursor: pointer;
  }
  .x:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .add {
    width: 9.5em;
    border-style: dashed;
  }
</style>
