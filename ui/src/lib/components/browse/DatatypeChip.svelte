<script lang="ts">
  // A typed literal's datatype as a small chip after its exact lexical form, the one place that
  // says how a datatype looks: its prefixed name, or its local name with no prefix, and the full
  // IRI as its title. `literalDatatype` decides which literals have one. The no-break space keeps
  // the chip with the last word of its value.
  import { localName, shorten, WELL_KNOWN, type PrefixMap } from '$lib/rdf';

  let { iri, prefixes = WELL_KNOWN }: { iri: string | null; prefixes?: PrefixMap } = $props();
</script>

{#if iri}&nbsp;<span class="lang-tag datatype" title={iri}
    ><span class="sr-only">datatype </span>{shorten(iri, prefixes) ?? localName(iri)}</span
  >{/if}

<style>
  .datatype {
    margin-left: 0;
    white-space: nowrap;
  }
</style>
