<script lang="ts">
  // In other datasets: for each other dataset the caller can read, whether and in which
  // graphs it mentions the IRI, with its label there and a link to its page in that
  // dataset. The page reads them after it renders, one query per dataset. The section shows
  // once it has an answer about this term, so a server with no other dataset has none.
  import type { Reuse } from '$lib/browse';

  let {
    found,
    checked,
    loading = false,
    href,
  }: {
    found: Reuse[];
    /** The datasets that were asked. */
    checked: string[];
    loading?: boolean;
    href: (dataset: string) => string;
  } = $props();
</script>

{#if !loading && checked.length}
  <section class="term-sec" aria-labelledby="reuse-h">
    <h2 id="reuse-h">
      In other datasets {#if found.length}<span class="count-badge">{found.length}</span>{/if}
    </h2>
    {#if !found.length}
      <p class="muted">
        No other dataset on this server mentions this IRI. <span class="faint"
          >Checked {checked.join(', ')}.</span
        >
      </p>
    {:else}
      <ul class="term-links">
        {#each found as r (r.dataset)}
          <li>
            <a href={href(r.dataset)}>{r.dataset}</a>
            {#if r.label}<span>as “{r.label}”</span>{/if}
            <span class="faint"
              >{r.graphs
                .map((g) => `${g.graph || 'default graph'} (${g.count.toLocaleString('en-US')})`)
                .join(', ')}</span
            >
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}
