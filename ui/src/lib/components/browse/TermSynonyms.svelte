<script lang="ts">
  // The synonyms of an entity by kind: exact, related, broad and narrow, and the SKOS
  // alternative and hidden labels, each value with its language and sources.
  import type { Linker, Sections } from '$lib/browse';
  import { localName } from '$lib/rdf';
  import SourcedValue from './SourcedValue.svelte';

  let {
    synonyms,
    linker,
    typeName = localName,
  }: {
    synonyms: Sections['synonyms'];
    linker: Linker;
    /** What a synonym type is called, such as "plural" (`synonymTypeName`). */
    typeName?: (iri: string) => string;
  } = $props();
</script>

{#if synonyms.length}
  <section class="term-sec" aria-labelledby="syn-h">
    <h2 id="syn-h">Synonyms</h2>
    <dl class="syn">
      {#each synonyms as g (g.kind)}
        <dt>{g.kind}</dt>
        <dd>
          {#each g.values as v, i (i)}<SourcedValue {v} {linker} {typeName} />{/each}
        </dd>
      {/each}
    </dl>
  </section>
{/if}

<style>
  .syn {
    display: grid;
    grid-template-columns: 112px minmax(0, 1fr);
    gap: 6px var(--sp-4);
    margin: 0;
  }
  dt {
    color: var(--text-2);
    font-size: var(--fs-sm);
    padding-top: 1px;
  }
  dd {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    min-width: 0;
  }
  @container browse (max-width: 639px) {
    .syn {
      grid-template-columns: minmax(0, 1fr);
      gap: 2px;
    }
    dd {
      margin-bottom: 6px;
    }
  }
</style>
