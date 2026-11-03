<script lang="ts">
  // A text whose CURIEs, such as GO:0032047 in a GO comment, link to their pages.
  import { linkCuries, type Linker } from '$lib/browse';

  let {
    text,
    linker,
    curieIri = () => null,
  }: { text: string; linker: Linker; curieIri?: (curie: string) => string | null } = $props();
  const parts = $derived(linkCuries(text, curieIri));
</script>

{#each parts as p, i (i)}{#if p.iri && linker.href(p.iri)}<a
      href={linker.href(p.iri)}
      title={linker.name(p.iri)}>{p.text}</a
    >{:else}{p.text}{/if}{/each}
