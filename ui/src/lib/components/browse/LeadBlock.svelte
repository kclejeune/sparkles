<script lang="ts">
  // The lead of an entity's Definition card (ROADMAP item 633): every definition,
  // description, comment and scope note, under a small heading per kind as the Notes under
  // it have, without their quote bar. The Definition's text is a step larger. In each kind
  // the first few values in the reader's languages show and the rest fold behind "N more",
  // and the translations fold behind "In N other languages". A value in another language
  // than the reader's best names it.
  import { langRank, LEAD_SHOWN, type Lead, type Linker, type Sourced } from '$lib/browse';
  import type { PrefixMap } from '$lib/rdf';
  import PredicateName from './PredicateName.svelte';
  import SourcedValue from './SourcedValue.svelte';

  let {
    lead,
    linker,
    langs,
    curieIri,
    prefixes,
    predicateHref,
  }: {
    lead: Lead;
    linker: Linker;
    langs: readonly string[];
    curieIri?: (curie: string) => string | null;
    prefixes: PrefixMap;
    /** The page of a predicate the headings name, when it has one. */
    predicateHref?: (iri: string) => string | null;
  } = $props();

  const foreign = (v: Sourced) => !!v.lang && langRank(v.lang, langs) > 0;
</script>

{#snippet entry(v: Sourced, cls: string)}<p class={cls}>
    <SourcedValue {v} {linker} {curieIri} showLang={foreign(v)} />
  </p>{/snippet}

{#if lead.groups.length}
  <div class="lead">
    {#each lead.groups as g, gi (g.predicates.join(' '))}
      {@const cls = g.kind === 'Definition' ? 'entry big' : 'entry'}
      <div class="group" role="group" aria-labelledby="lead-h-{gi}" data-kind={g.kind}>
        <h3 id="lead-h-{gi}">
          {g.kind}<PredicateName predicates={g.predicates} {prefixes} href={predicateHref} />
        </h3>
        {#each g.values.slice(0, LEAD_SHOWN) as v, i (i)}{@render entry(
            v,
            gi === 0 && i === 0 ? `${cls} lede` : cls,
          )}{/each}
        {#if g.values.length > LEAD_SHOWN}
          <details class="fold">
            <summary>{g.values.length - LEAD_SHOWN} more</summary>
            {#each g.values.slice(LEAD_SHOWN) as v, i (i)}{@render entry(v, cls)}{/each}
          </details>
        {/if}
        {#if g.translations.length}
          <details class="fold">
            <summary
              >In {g.translations.length} other {g.translations.length === 1
                ? 'language'
                : 'languages'}</summary
            >
            {#each g.translations as v, i (i)}{@render entry(v, cls)}{/each}
          </details>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  /* the kinds stand apart by twice the gap that splits a kind's own values */
  .lead {
    display: grid;
    gap: var(--sp-4);
    min-width: 0;
  }
  .group {
    display: grid;
    gap: var(--sp-2);
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .entry {
    margin: 0;
    font-size: var(--fs-md);
    line-height: 1.55;
  }
  .entry.big {
    font-size: 15px;
    line-height: 1.6;
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
  .fold .entry + .entry {
    margin-top: var(--sp-2);
  }
</style>
