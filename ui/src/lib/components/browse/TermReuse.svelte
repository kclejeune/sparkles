<script lang="ts">
  // In other datasets: for each other dataset the caller can read, whether and in which
  // graphs it mentions the IRI, with its label there and a link to its page in that
  // dataset. The page reads them after it renders, one query per dataset.
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

<section class="term-sec" aria-labelledby="reuse-h">
  <h2 id="reuse-h">
    In other datasets {#if found.length}<span class="count-badge">{found.length}</span>{/if}
  </h2>
  {#if loading}
    <p class="faint">
      Looking in {checked.length} other {checked.length === 1 ? 'dataset' : 'datasets'}…
    </p>
  {:else if !checked.length}
    <p class="muted">This server has no other dataset you can read.</p>
  {:else if !found.length}
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
