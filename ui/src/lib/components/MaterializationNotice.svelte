<script lang="ts">
  import type { MaterializationSource } from '$lib/inferred-read';
  let { source }: { source: MaterializationSource } = $props();
</script>

<div class="materialization-notice">
  <p>
    Materialized {source.profile.toUpperCase()} facts at commit {source.commit}, produced at commit {source.producerCommit}.
    Completeness and consistency are unknown. Explanations are unavailable.
  </p>
  <details>
    <summary>Materialization source</summary>
    <p class="mono small">{source.sourceId}</p>
    <p>
      {source.inputGraphCount} input graphs{source.inputGraphsTruncated ? ' (sample shown)' : ''}.
    </p>
    <ul>
      {#each source.inputGraphs as graph (graph)}<li><code>{graph}</code></li>{/each}
    </ul>
    {#if source.warningCount}
      <p>
        {source.warningCount} producer warnings{source.warningsTruncated ? ' (sample shown)' : ''}.
      </p>
      <ul>
        {#each source.warnings as warning, i (i)}<li>{warning}</li>{/each}
      </ul>
    {/if}
  </details>
</div>

<style>
  .materialization-notice {
    font-size: var(--font-sm);
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  p {
    margin: var(--sp-2) 0;
  }
</style>
