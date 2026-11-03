<script lang="ts">
  // The logical definition of an entity: its axioms in the sections of the editor's frame
  // (Subclass of, Equivalent to, Domains, Characteristics and the rest), each a line of
  // linked Manchester prose. Axioms of a form no section takes are shown in OWL functional
  // syntax.
  import type { AxiomGroup, Linker } from '$lib/browse';
  import type { PrefixMap } from '$lib/rdf';
  import AxiomProse from './AxiomProse.svelte';

  let {
    groups,
    linker,
    prefixes,
    title = 'Logical definition',
  }: { groups: AxiomGroup[]; linker: Linker; prefixes: PrefixMap; title?: string } = $props();

  /** Sections whose axioms pair a property with a value, as in a fact. */
  const PAIRS = new Set(['objectAssertions', 'dataAssertions', 'negative']);
</script>

{#if groups.length}
  <section class="term-sec" aria-labelledby="logic-h">
    <h2 id="logic-h">{title}</h2>
    {#each groups as g (g.id)}
      <h3>{g.title}</h3>
      <ul class="ax" data-section={g.id}>
        {#each g.items as it, i (i)}
          <li>
            {#if it.word}
              {it.word}
            {:else if g.id === 'other'}
              <code class="ofn">{it.axiom.ofn}</code>
            {:else if PAIRS.has(g.id)}
              {#if g.id === 'negative'}<span class="kw">not</span>{' '}{/if}<AxiomProse
                value={it.values[0]}
                {linker}
                {prefixes}
              />{' '}<AxiomProse value={it.values[1]} {linker} {prefixes} />
            {:else}
              {#each it.values as v, j (j)}{#if j > 0},{' '}{/if}<AxiomProse
                  value={v}
                  {linker}
                  {prefixes}
                />{/each}
            {/if}
          </li>
        {/each}
      </ul>
    {/each}
  </section>
{/if}

<style>
  .ax {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  .ax li {
    font-size: var(--fs-md);
    line-height: 1.5;
    padding-left: var(--sp-4);
    position: relative;
    min-width: 0;
  }
  .ax li::before {
    content: '';
    position: absolute;
    left: 2px;
    top: 0.65em;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--border-strong);
  }
  .kw {
    color: var(--kw);
    font-weight: 600;
    font-style: italic;
  }
  .ofn {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
</style>
