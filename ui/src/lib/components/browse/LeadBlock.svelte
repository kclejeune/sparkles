<script lang="ts">
  // The lead of an entity's Definition card (ROADMAP item 633): every definition,
  // description, comment and scope note, each with the chips of the properties that state
  // it. The best leads at the reading size and the others in the reader's languages follow
  // a step smaller, the first few shown and the rest behind "N more". The translations of
  // each kind fold behind one "In N other languages". A value in another language than the
  // reader's best names its language.
  import { langRank, LEAD_SHOWN, type Lead, type Linker, type Sourced } from '$lib/browse';
  import SourcedValue from './SourcedValue.svelte';

  let {
    lead,
    linker,
    langs,
    curieIri,
  }: {
    lead: Lead;
    linker: Linker;
    langs: readonly string[];
    curieIri?: (curie: string) => string | null;
  } = $props();

  const shown = $derived(lead.more.slice(0, LEAD_SHOWN - 1));
  const folded = $derived(lead.more.slice(LEAD_SHOWN - 1));
  const foreign = (v: Sourced) => !!v.lang && langRank(v.lang, langs) > 0;
</script>

{#snippet entry(v: Sourced & { chips: string[] })}{#each v.chips as c (c)}<span
      class="badge lead-chip">{c}</span
    >{/each}<SourcedValue {v} {linker} {curieIri} showLang={foreign(v)} />{/snippet}

{#if lead.first}
  <div class="lead">
    <p class="lede">{@render entry(lead.first)}</p>
    {#each shown as v, i (i)}<p class="more">{@render entry(v)}</p>{/each}
    {#if folded.length}
      <details class="fold">
        <summary>{folded.length} more</summary>
        {#each folded as v, i (i)}<p class="more">{@render entry(v)}</p>{/each}
      </details>
    {/if}
    {#each lead.translations as g (g.kind)}
      <details class="fold" data-kind={g.kind}>
        <summary
          ><span class="badge lead-chip">{g.kind}</span>In {g.values.length} other {g.values
            .length === 1
            ? 'language'
            : 'languages'}</summary
        >
        {#each g.values as v, i (i)}<p class="more">{@render entry(v)}</p>{/each}
      </details>
    {/each}
  </div>
{/if}

<style>
  .lead {
    display: grid;
    gap: var(--sp-2);
    min-width: 0;
  }
  .lede {
    margin: 0;
    font-size: 15px;
    line-height: 1.6;
  }
  .more {
    margin: 0;
    font-size: var(--fs-md);
    line-height: 1.55;
    color: var(--text-2);
  }
  .lead-chip {
    margin-right: var(--sp-2);
    vertical-align: 2px;
  }
  .fold summary {
    cursor: pointer;
    color: var(--text-2);
    font-size: var(--fs-sm);
    font-weight: 500;
  }
  .fold[open] summary {
    margin-bottom: var(--sp-2);
  }
  .fold .more + .more {
    margin-top: var(--sp-2);
  }
</style>
