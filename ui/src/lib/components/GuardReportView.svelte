<script lang="ts">
  // A write guard's report: the results that refused (or would refuse) a write.
  import type * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import { guardRows, guardSummary } from '$lib/merge-page';
  import type { PrefixMap } from '$lib/rdf';

  let {
    report,
    prefixes = {},
  }: {
    report: api.GuardReport;
    prefixes?: PrefixMap;
  } = $props();

  const rows = $derived(guardRows(report, prefixes));
</script>

<div class="guard">
  <p>
    <strong>{guardSummary(report)}</strong>
    {#if report.language}<span class="faint">({report.language === 'shex' ? 'ShEx' : 'SHACL'})</span
      >{/if}
  </p>
  {#if rows.length}
    <div class="results">
      <table class="data" aria-label="Validation results">
        <thead>
          <tr>
            <th>Focus node</th>
            <th>Path</th>
            <th>Message</th>
            <th>Shape</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r, i (i)}
            <tr>
              <td class="mono">{r.node}</td>
              <td class="mono">{r.path}</td>
              <td>{r.message}</td>
              <td class="mono">{r.shape}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
  {#if report.truncated}
    <p class="faint">The report lists the first {fmtInt(rows.length)} results.</p>
  {/if}
</div>

<style>
  .guard {
    display: grid;
    gap: 6px;
  }
  .guard p {
    margin: 0;
  }
  .results {
    max-height: 260px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .results table {
    font-size: 12px;
  }
  .results td {
    vertical-align: top;
    overflow-wrap: anywhere;
  }
</style>
