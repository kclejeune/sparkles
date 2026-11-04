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
    <dl class="term-rows">
      {#each synonyms as g (g.kind)}
        <div class="term-row">
          <dt>{g.kind}</dt>
          <dd class="values">
            {#each g.values as v, i (i)}<SourcedValue {v} {linker} {typeName} />{/each}
          </dd>
        </div>
      {/each}
    </dl>
  </section>
{/if}
