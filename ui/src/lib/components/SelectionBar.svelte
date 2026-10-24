<script lang="ts">
  // The bar under a view's toolbar that names the shared selection and its source, with
  // Clear, Use in query, which writes the selected IRIs into the query as a `VALUES` block,
  // and, where the page can draw entities, Open as graph (U11 section 2.1). `Escape`
  // clears the selection from any view that has no dialog or field open.
  import { fmtInt } from '$lib/format';
  import { selection, SELECTION_CAP } from '$lib/selection.svelte';

  let {
    onopengraph,
    onuse,
    vars = [],
    usevar = $bindable(''),
    onvarpick,
  }: {
    /** Draw the selected entities (term keys) in Explore. */
    onopengraph?: (ids: string[]) => void;
    /** Write the selected IRIs (term keys) into the query, bound to `v` when vars are given. */
    onuse?: (ids: string[], v: string) => void;
    /** The variables a `VALUES` block may bind, for the choice beside Use in query. */
    vars?: string[];
    /** The variable chosen. */
    usevar?: string;
    /** The person chose the variable, which then stops following the selection. */
    onvarpick?: () => void;
  } = $props();

  const iris = $derived([...selection.ids].filter((id) => id.startsWith('<')));

  function onkey(e: KeyboardEvent) {
    if (e.key !== 'Escape' || e.defaultPrevented || !selection.active) return;
    const t = e.target as HTMLElement | null;
    if (t?.closest?.('input, textarea, select, [contenteditable], dialog, [role="dialog"]')) return;
    selection.clear();
  }
</script>

<svelte:window onkeydown={onkey} />

{#if selection.active}
  <div class="selbar" role="region" aria-label="Selection" data-size={selection.size}>
    <span class="what"
      ><strong>{selection.size ? fmtInt(selection.size) : 'None'}</strong> selected{selection.source
        ? ` by ${selection.source}`
        : ''}{#if selection.dropped}
        <span class="faint"
          >, the first {fmtInt(SELECTION_CAP)} of {fmtInt(selection.size + selection.dropped)}</span
        >{/if}</span
    >
    <span class="spacer"></span>
    <button class="btn sm" onclick={() => selection.clear()}>Clear</button>
    {#if onuse}
      <span class="use">
        <button
          class="btn sm"
          disabled={!iris.length || (vars.length > 0 && !usevar)}
          title="Write the selected entities into the query as a VALUES block"
          onclick={() => onuse?.(iris, usevar)}>Use in query</button
        >
        {#if vars.length}
          <label class="as"
            >as
            <select
              aria-label="The variable of the VALUES block"
              bind:value={usevar}
              onchange={() => onvarpick?.()}
            >
              {#each vars as v (v)}<option value={v}>?{v}</option>{/each}
            </select></label
          >
        {/if}
      </span>
    {/if}
    {#if onopengraph}
      <button
        class="btn sm"
        title="Draw the selected entities in the graph"
        onclick={() => onopengraph?.([...selection.ids])}>Open as graph</button
      >
    {/if}
  </div>
{/if}

<style>
  .selbar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px 10px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--spark-soft);
    font-size: var(--fs-sm);
  }
  .use,
  .as {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .as select {
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .as select {
      min-height: 44px;
    }
    .selbar :global(.btn) {
      min-height: 44px;
      min-width: 44px;
    }
  }
</style>
