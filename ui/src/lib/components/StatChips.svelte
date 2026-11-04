<script lang="ts" module>
  /** One count of a chip row: its figure and the words after it, and where it leads. */
  export type StatChip = {
    key: string;
    /** the figure, already formatted, such as "1,204" or "12 MB" */
    value: string;
    /** the words after the figure, such as "quads" or "on disk" */
    label: string;
    href?: string | null;
    title?: string;
  };
</script>

<script lang="ts">
  // A page's headline counts as one row of chips, each its figure and what it counts, and a
  // link to the section it counts when it has one (ROADMAP item 633). The row is a navigation
  // landmark when a chip links, and a list otherwise. The styles are `.stat-chips` in app.css.
  let { items, label = 'Counts' }: { items: StatChip[]; label?: string } = $props();
  const links = $derived(items.some((c) => c.href));
</script>

{#snippet chip(c: StatChip)}{#if c.href}<a class="stat-chip" href={c.href} title={c.title}
      ><b>{c.value}</b> {c.label}</a
    >{:else}<span class="stat-chip" title={c.title}><b>{c.value}</b> {c.label}</span>{/if}{/snippet}

{#if items.length}
  {#if links}
    <nav class="stat-chips" aria-label={label}>
      {#each items as c (c.key)}{@render chip(c)}{/each}
    </nav>
  {:else}
    <ul class="stat-chips" aria-label={label}>
      {#each items as c (c.key)}<li>{@render chip(c)}</li>{/each}
    </ul>
  {/if}
{/if}
