<script lang="ts">
  // The summary of an answer (C18 §6.2). Each [n] marker links to the row it cites, a row
  // under the pointer lights the markers that cite it, and the summary is labelled as
  // generated with the number of rows it cites. It is only shown above its rows.
  import type { AskSummary } from '$lib/ask-api';
  import { summaryLabel, summaryParts } from '$lib/ask';
  import Icon from './Icon.svelte';

  let {
    summary,
    rows,
    collapsed = $bindable(false),
    hoverRow,
    oncite,
  }: {
    summary: AskSummary;
    /** The rows of the result on screen. */
    rows: number;
    collapsed: boolean;
    /** The row under the pointer in the table (0-based). */
    hoverRow: number | null;
    /** A marker was chosen: scroll to this row (1-based). */
    oncite: (row: number) => void;
  } = $props();

  const parts = $derived(summaryParts(summary.text, summary.citations));
  const label = $derived(summaryLabel(summary, rows));
  const model = $derived(summary.model ? `${summary.provider ?? ''} · ${summary.model}` : '');
</script>

<section class="answer" aria-label="Answer">
  <div class="head">
    <span class="key">Answer</span>
    {#if !collapsed}
      <p class="text">
        {#each parts as p, i (i)}{#if p.rows}<button
              class="cite"
              class:lit={hoverRow != null && p.rows.includes(hoverRow + 1)}
              title="Row {p.rows.join(', ')}"
              onclick={() => oncite(p.rows![0])}>{p.text}</button
            >{:else}{p.text}{/if}{/each}
      </p>
    {:else}
      <span class="faint small text">The summary is collapsed.</span>
    {/if}
    <button
      class="btn ghost icon sm"
      aria-expanded={!collapsed}
      aria-label={collapsed ? 'Show the summary' : 'Collapse the summary'}
      title={collapsed ? 'Show the summary' : 'Collapse the summary'}
      onclick={() => (collapsed = !collapsed)}
      ><Icon name={collapsed ? 'chevron' : 'chevronDown'} size={12} /></button
    >
  </div>
  {#if !collapsed}
    <div class="meta faint small">
      <span>{label}</span>
      {#if model}<span class="mono" title="The model that wrote the summary">{model}</span>{/if}
      {#if !new Set(summary.citations).size}<span
          >The summary cites no row, so check it against the table.</span
        >{/if}
    </div>
  {/if}
</section>

<style>
  .answer {
    display: grid;
    gap: 2px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--iri) 5%, var(--surface));
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    min-width: 0;
  }
  .key {
    flex: none;
    font-weight: 600;
  }
  .text {
    flex: 1;
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .cite {
    all: unset;
    cursor: pointer;
    color: var(--iri);
    font-weight: 600;
    border-radius: 3px;
    padding: 0 1px;
  }
  .cite:hover,
  .cite.lit {
    background: color-mix(in srgb, var(--iri) 18%, transparent);
  }
  .cite:focus-visible {
    box-shadow: 0 0 0 2px var(--iri);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    padding-left: 58px;
  }
</style>
