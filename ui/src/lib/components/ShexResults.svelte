<script lang="ts">
  // The result map of a ShEx validation: one row per association (node, shape, status,
  // first reason); a row with failures or prints expands into them.
  import type { ShexReport, ShexResult } from '$lib/api';
  import { fmtInt } from '$lib/format';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { countsLine, failureRow, shapeText } from '$lib/shex';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    report,
    prefixes,
    explore,
  }: {
    report: ShexReport;
    prefixes: PrefixMap;
    /** The Explore link of an IRI. */
    explore: (iri: string) => string;
  } = $props();

  let shown = $state(50);
  let open = $state(new Set<number>());
  $effect(() => {
    void report;
    shown = 50;
    open = new Set();
  });

  function toggle(i: number) {
    const next = new Set(open);
    if (!next.delete(i)) next.add(i);
    open = next;
  }

  const expandable = (r: ShexResult) =>
    (r.appinfo?.failures.length ?? 0) > 0 || (r.appinfo?.prints?.length ?? 0) > 0;
</script>

{#if report.warnings.length}
  <ul class="warnings">
    {#each report.warnings as w, i (i)}
      <li><Icon name="alert" size={12} /> {w}</li>
    {/each}
  </ul>
{/if}
<p class="faint counts">{countsLine(report.counts, report.millis)}</p>
{#if report.results.length}
  <div class="shex-results">
    <table class="data">
      <thead>
        <tr><th>Node</th><th>Shape</th><th>Status</th><th>Reason</th></tr>
      </thead>
      <tbody>
        {#each report.results.slice(0, shown) as r, i (i)}
          {@const more = expandable(r)}
          <tr class:open={open.has(i)}>
            <td class="mono cell">
              {#if more}
                <button
                  class="toggle"
                  aria-expanded={open.has(i)}
                  aria-label="{open.has(i) ? 'Hide' : 'Show'} the failures"
                  onclick={() => toggle(i)}><Icon name="chevron" size={12} /></button
                >
              {/if}
              {#if r.node.type === 'uri'}
                {@const iri = r.node.value}
                <a class="node t-iri" href={explore(iri)} title="{iri}  (open in Explore)"
                  >{displayIri(iri, prefixes)}</a
                >
              {:else}
                <TermView term={r.node} {prefixes} />
              {/if}
            </td>
            <td class="mono cell" title={r.shape.type === 'uri' ? r.shape.value : undefined}
              >{shapeText(r.shape, prefixes)}</td
            >
            <td>
              <span class="badge {r.status === 'nonconformant' ? 'danger' : 'ok'}">{r.status}</span>
            </td>
            <td class="cell reason" title={r.reason}>{r.reason ?? ''}</td>
          </tr>
          {#if more && open.has(i)}
            <tr class="detail">
              <td colspan="4">
                {#if r.appinfo?.failures.length}
                  <table class="failures">
                    <thead>
                      <tr><th>Failure</th><th>Predicate</th><th>Value</th><th>Detail</th></tr>
                    </thead>
                    <tbody>
                      {#each r.appinfo.failures as f, j (j)}
                        {@const row = failureRow(f, prefixes)}
                        <tr>
                          <td class="kind">{row.kind}</td>
                          <td class="mono t-iri">{row.predicate ?? ''}</td>
                          <td class="mono">
                            {#if row.value}<TermView term={row.value} {prefixes} />{/if}
                          </td>
                          <td class="mono">{row.detail}</td>
                        </tr>
                      {/each}
                    </tbody>
                  </table>
                {/if}
                {#if r.appinfo?.prints?.length}
                  <pre class="prints">{r.appinfo.prints.join('\n')}</pre>
                {/if}
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
    {#if report.results.length > shown}
      <button class="btn ghost sm more" onclick={() => (shown += 100)}
        >Show more ({fmtInt(report.results.length - shown)} hidden)</button
      >
    {/if}
  </div>
{/if}

<style>
  .counts {
    margin: 0 14px 10px;
    font-size: var(--fs-sm);
  }
  .warnings {
    list-style: none;
    margin: 0 14px 8px;
    padding: 8px 10px;
    display: grid;
    gap: 4px;
    border-radius: var(--r);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    color: var(--warn);
    font-size: var(--fs-sm);
  }
  .shex-results {
    border-top: 1px solid var(--border);
    overflow-x: auto;
  }
  .shex-results > table {
    table-layout: fixed;
    min-width: 520px;
  }
  .shex-results th:nth-child(1) {
    width: 30%;
  }
  .shex-results th:nth-child(2) {
    width: 20%;
  }
  .shex-results th:nth-child(3) {
    width: 15%;
  }
  .cell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .reason {
    color: var(--text-2);
  }
  .toggle {
    all: unset;
    cursor: pointer;
    display: inline-flex;
    vertical-align: -2px;
    margin-right: 4px;
    color: var(--text-3);
    transition: transform 0.15s;
  }
  .toggle:focus-visible {
    box-shadow: var(--focus);
  }
  tr.open .toggle {
    transform: rotate(90deg);
  }
  .node {
    color: var(--iri);
    text-decoration: none;
  }
  .node:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  tr.detail > td {
    background: var(--surface-2);
    padding: 6px 10px 10px 28px;
  }
  .failures {
    width: 100%;
    font-size: 12px;
  }
  .failures th {
    font-weight: 500;
    color: var(--text-2);
    text-align: left;
  }
  .failures td,
  .failures th {
    padding: 3px 8px 3px 0;
    vertical-align: top;
  }
  .failures .kind {
    white-space: nowrap;
  }
  .prints {
    margin: 6px 0 0;
    font-size: 12px;
    white-space: pre-wrap;
  }
  .more {
    margin: 4px 8px 8px;
  }
</style>
