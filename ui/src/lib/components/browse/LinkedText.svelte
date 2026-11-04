<script lang="ts">
  // A text whose CURIEs, such as GO:0032047 in a GO comment, link to their pages. Blank
  // lines in the text start a new paragraph and single newlines a new line, so a
  // description of several paragraphs keeps its shape (`textBlocks`).
  import { linkCuries, textBlocks, type Linker } from '$lib/browse';

  let {
    text,
    linker,
    curieIri = () => null,
  }: { text: string; linker: Linker; curieIri?: (curie: string) => string | null } = $props();
  const blocks = $derived(
    textBlocks(text).map((para) => para.map((line) => linkCuries(line, curieIri))),
  );
</script>

{#each blocks as para, pi (pi)}{#if pi > 0}<span class="para-gap" aria-hidden="true"
    ></span>{/if}{#each para as parts, li (li)}{#if li > 0}<br
      />{/if}{#each parts as p, i (i)}{#if p.iri && linker.href(p.iri)}<a
          href={linker.href(p.iri)}
          title={linker.name(p.iri)}>{p.text}</a
        >{:else}{p.text}{/if}{/each}{/each}{/each}

<style>
  /* a paragraph break inside the inline value, a half line of space */
  .para-gap {
    display: block;
    height: 0.5em;
  }
</style>
