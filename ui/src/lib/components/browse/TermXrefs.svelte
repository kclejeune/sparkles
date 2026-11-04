<script lang="ts">
  // Cross-references: `oboInOwl:hasDbXref` by database, with the well-known databases as
  // links, then `rdfs:seeAlso` and `rdfs:isDefinedBy`, and the five SKOS mapping relations.
  import { valueText, isLiteral, type Linker, type Sections } from '$lib/browse';

  let {
    xrefs,
    seeAlso,
    mappings,
    linker,
    here = true,
  }: {
    xrefs: Sections['xrefs'];
    seeAlso: Sections['seeAlso'];
    mappings: Sections['mappings'];
    linker: Linker;
    /** The cross-references show here. Otherwise a panel of the facts has them, and only the
     * see-also links remain. */
    here?: boolean;
  } = $props();
  const external = (v: string) => /^https?:\/\//.test(v);
</script>

{#if (here && xrefs.length) || seeAlso.length}
  <section class="term-sec" aria-labelledby="xref-h">
    <h2 id="xref-h">{here ? 'Cross-references' : 'See also'}</h2>
    {#if here && xrefs.length}
      <div class="term-flow xrefs">
        {#each xrefs as x (x.value)}
          {#if x.href}
            <a href={x.href} rel="noopener noreferrer" target="_blank"
              >{#if x.database}<span class="db">{x.database}:</span>{/if}{x.id}</a
            >
          {:else}
            <span
              >{#if x.database}<span class="db">{x.database}:</span>{/if}{x.id}</span
            >
          {/if}
        {/each}
      </div>
    {/if}
    {#if seeAlso.length}
      {#if here}<h3>See also</h3>{/if}
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
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
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

<style>
  .db {
    color: var(--text-3);
  }
  .xrefs a {
    color: var(--iri);
  }
</style>
