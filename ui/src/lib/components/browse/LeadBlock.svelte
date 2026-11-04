<script lang="ts">
  // The top of an entity's page: one text, the first definition or else the first
  // description, comment or note, at a larger size with its predicate after it. Every other
  // definition, description, comment and note waits behind one control that says what it holds,
  // as tight rows of text by kind, each with its kind and predicate small after it. The lead's
  // translations fold behind "In N other languages". A value in another language than the
  // reader's best names it.
  import {
    langRank,
    languageCount,
    leadRest,
    leadRestSummary,
    type Lead,
    type Linker,
    type Sections,
    type Sourced,
  } from '$lib/browse';
  import type { PrefixMap } from '$lib/rdf';
  import PredicateName from './PredicateName.svelte';
  import SourcedValue from './SourcedValue.svelte';

  let {
    lead,
    notes = [],
    linker,
    langs,
    curieIri,
    prefixes,
    predicateHref,
    open = false,
  }: {
    lead: Lead;
    /** The notes, which wait behind the control with the other texts. */
    notes?: Sections['notes'];
    linker: Linker;
    langs: readonly string[];
    curieIri?: (curie: string) => string | null;
    prefixes: PrefixMap;
    /** The page of a predicate a text names, when it has one. */
    predicateHref?: (iri: string) => string | null;
    /** The other texts start open, as they do in edit mode. */
    open?: boolean;
  } = $props();

  const foreign = (v: Sourced) => !!v.lang && langRank(v.lang, langs) > 0;
  const first = $derived(lead.groups[0]);
  const rows = $derived(leadRest(lead, notes, langs));
  const summary = $derived(leadRestSummary(rows, first?.kind ?? ''));
</script>

{#snippet source(kind: string | null, predicates: string[])}
  <span class="source"
    >{#if kind}{kind}{/if}<PredicateName
      {predicates}
      {prefixes}
      href={predicateHref}
      dot={!!kind}
    /></span
  >
{/snippet}

{#if first || rows.length}
  <div class="lead">
    {#if first?.values.length}
      <p class="lede">
        <SourcedValue v={first.values[0]} {linker} {curieIri} showLang={foreign(first.values[0])} />
        {@render source(first.kind === 'Definition' ? null : first.kind, first.predicates)}
      </p>
    {/if}
    {#if first?.translations.length}
      {@const n = languageCount(first.translations)}
      <details class="fold">
        <summary>In {n} other {n === 1 ? 'language' : 'languages'}</summary>
        {#each first.translations as v, i (i)}
          <p class="entry"><SourcedValue {v} {linker} {curieIri} showLang={foreign(v)} /></p>
        {/each}
      </details>
    {/if}
    {#if rows.length}
      <details class="fold rest" {open}>
        <summary>{summary}</summary>
        <ul>
          {#each rows as g (g.predicates.join(' ') + g.kind)}
            {#each g.values as v, i (i)}
              <li data-kind={g.kind} class:starts={i === 0}>
                <SourcedValue {v} {linker} {curieIri} showLang={foreign(v)} />
                {@render source(g.kind, g.predicates)}
              </li>
            {/each}
          {/each}
        </ul>
      </details>
    {/if}
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
    font-size: 16px;
    line-height: 1.55;
    max-width: 75ch;
  }
  /* after the text, and on a line of its own when it does not fit, never split at its dot */
  .source {
    display: inline-block;
    max-width: 100%;
    margin-left: 0.25em;
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .lede .source {
    font-size: var(--fs-sm);
  }
  .entry {
    margin: var(--sp-1) 0 0;
    font-size: var(--fs-md);
    line-height: 1.5;
    max-width: 75ch;
  }
  .fold summary {
    cursor: pointer;
    color: var(--text-2);
    font-size: var(--fs-sm);
    font-weight: 500;
  }
  ul {
    list-style: none;
    margin: var(--sp-2) 0 0;
    padding: 0;
    display: grid;
    gap: var(--sp-1);
  }
  li {
    font-size: var(--fs-md);
    line-height: 1.5;
    max-width: 75ch;
    min-width: 0;
  }
  li.starts:not(:first-child) {
    margin-top: var(--sp-2);
  }
</style>
