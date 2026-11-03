<script lang="ts">
  // The labels of a SKOS resource by language: preferred and alternative labels in a
  // table, with the hidden labels behind a toggle.
  import type { Sections } from '$lib/browse';

  let { labels }: { labels: Sections['labels'] } = $props();
  let hidden = $state(false);
  const hiddenCount = $derived(labels.reduce((n, r) => n + r.hidden.length, 0));
  const rows = $derived(labels.filter((r) => r.preferred.length || r.alternative.length || hidden));
  const rtl = (lang: string) => /^(ar|he|fa|ur|yi|ps|sd|ug|dv)(-|$)/i.test(lang);
</script>

{#if labels.length}
  <section class="term-sec" aria-labelledby="labels-h">
    <h2 id="labels-h">Labels</h2>
    <div class="table-scroll">
      <table class="data">
        <thead>
          <tr>
            <th>Language</th><th>Preferred</th><th>Alternative</th>{#if hidden}<th>Hidden</th>{/if}
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.lang)}
            {@const dir = rtl(r.lang) ? 'rtl' : undefined}
            <tr>
              <td class="mono">{r.lang || '—'}</td>
              <td {dir}>{r.preferred.join(', ')}</td>
              <td {dir}>{r.alternative.join(', ')}</td>
              {#if hidden}<td {dir}>{r.hidden.join(', ')}</td>{/if}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if hiddenCount}
      <p class="faint hint">
        {#if hidden}Hidden labels are shown.{:else}{hiddenCount} hidden {hiddenCount === 1
            ? 'label is'
            : 'labels are'} not shown.{/if}
        <button class="more-btn" type="button" onclick={() => (hidden = !hidden)}
          >{hidden ? 'Hide them' : 'Show them'}</button
        >
      </p>
    {/if}
  </section>
{/if}

<style>
  .hint {
    margin-top: 6px;
    font-size: var(--fs-sm);
  }
</style>
