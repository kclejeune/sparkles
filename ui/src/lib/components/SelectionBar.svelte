<script lang="ts">
  // The bar under a view's toolbar that names the shared selection and its source, with
  // Clear and, where the page can draw entities, Open as graph (U11 section 2.1). `Escape`
  // clears the selection from any view that has no dialog or field open. Use in query
  // waits for the query builder's inline VALUES (U08 Phase 3).
  import { fmtInt } from '$lib/format';
  import { selection, SELECTION_CAP } from '$lib/selection.svelte';

  let {
    onopengraph,
  }: {
    /** Draw the selected entities (term keys) in Explore. */
    onopengraph?: (ids: string[]) => void;
  } = $props();

  function onkey(e: KeyboardEvent) {
    if (e.key !== 'Escape' || e.defaultPrevented || !selection.size) return;
    const t = e.target as HTMLElement | null;
    if (t?.closest?.('input, textarea, select, [contenteditable], dialog, [role="dialog"]')) return;
    selection.clear();
  }
</script>

<svelte:window onkeydown={onkey} />

{#if selection.size}
  <div class="selbar" role="region" aria-label="Selection" data-size={selection.size}>
    <span class="what"
      ><strong>{fmtInt(selection.size)}</strong> selected{selection.source
        ? ` by ${selection.source}`
        : ''}{#if selection.dropped}
        <span class="faint"
          >, the first {fmtInt(SELECTION_CAP)} of {fmtInt(selection.size + selection.dropped)}</span
        >{/if}</span
    >
    <span class="spacer"></span>
    <button class="btn sm" onclick={() => selection.clear()}>Clear</button>
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
  @media (max-width: 760px) {
    .selbar :global(.btn) {
      min-height: 44px;
      min-width: 44px;
    }
  }
</style>
