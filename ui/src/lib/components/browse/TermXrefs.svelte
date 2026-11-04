<script lang="ts">
  // The links of an entity that are not identifiers: `rdfs:seeAlso` and `rdfs:isDefinedBy`,
  // and the five SKOS mapping relations. TermIdentifiers shows the `oboInOwl:hasDbXref` ones.
  import { valueText, isLiteral, type Linker, type Sections } from '$lib/browse';
  import { literalDatatype } from '$lib/ontology';
  import { WELL_KNOWN, type PrefixMap } from '$lib/rdf';
  import DatatypeChip from './DatatypeChip.svelte';

  let {
    seeAlso,
    mappings,
    linker,
    prefixes = WELL_KNOWN,
  }: {
    seeAlso: Sections['seeAlso'];
    mappings: Sections['mappings'];
    linker: Linker;
    prefixes?: PrefixMap;
  } = $props();
  const external = (v: string) => /^https?:\/\//.test(v);
</script>

{#if seeAlso.length}
  <section class="term-sec" aria-labelledby="also-h">
    <h2 id="also-h">See also</h2>
    <ul class="term-links">
      {#each seeAlso as s, i (i)}
        {@const v = valueText(s.value)}
        <li>
          {#if !isLiteral(s.value) && linker.href(v) && !external(v)}
            <a href={linker.href(v)}>{linker.name(v)}</a>
          {:else if external(v)}
            <a href={v} rel="noopener noreferrer" target="_blank">{v}</a>
          {:else}
            <span>{v}</span>
          {/if}<DatatypeChip iri={literalDatatype(s.value)} {prefixes} />
        </li>
      {/each}
    </ul>
  </section>
{/if}
{#if mappings.length}
  <section class="term-sec" aria-labelledby="map-h">
    <h2 id="map-h">Mappings</h2>
    {#each mappings as g (g.kind)}
      <h3>{g.kind}</h3>
      <div class="term-flow">
        {#each g.values as v (v)}<a href={linker.href(v) ?? v}>{linker.name(v)}</a>{/each}
      </div>
    {/each}
  </section>
{/if}
