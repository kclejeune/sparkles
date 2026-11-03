<script lang="ts">
  // The downloads of an entity: what the graphs say about it in Turtle, RDF/XML, OWL
  // functional syntax and JSON-LD, from the term route's `format`.
  import { DOWNLOADS } from '$lib/browse';
  import Icon from '$components/Icon.svelte';

  let {
    href,
    noun = 'this entity',
    formats = DOWNLOADS,
  }: {
    /** The address of the download in a format. */
    href: (format: string) => string;
    noun?: string;
    formats?: { format: string; label: string }[];
  } = $props();
</script>

<div class="downloads">
  <span class="what">Download {noun}</span>
  {#each formats as f (f.format)}
    <a class="btn sm" href={href(f.format)} download data-format={f.format}
      ><Icon name="download" size={14} />{f.label}</a
    >
  {/each}
</div>

<style>
  .downloads {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .what {
    width: 100%;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .btn {
    font-size: var(--fs-sm);
  }
</style>
