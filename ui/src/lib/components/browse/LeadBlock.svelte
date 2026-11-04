<script lang="ts">
  // The top of an entity's page: one text, the first definition or else the first description,
  // comment or note, at a larger size. Under it one quiet disclosure for each kind of the other
  // texts, its lead's translations, more definitions, descriptions, comments and notes, on one
  // wrapping row with what each opens below the row. Every value ends with a chip of the property
  // that states it, and the kind a chip does not spell out stays for a screen reader. A value in
  // another language than the reader's best names it.
  import {
    langRank,
    languageCount,
    leadDisclosures,
    leadRest,
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
    /** The notes, which wait behind their own disclosure. */
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
  const more = $derived(leadDisclosures(leadRest(lead, notes, langs), first?.kind ?? ''));
</script>

<!-- the no-break space keeps a chip with the last word of its text, so that it never sits alone -->
{#snippet value(v: Sourced, kind: string | null, predicates: string[])}
  {#if kind}<span class="sr-only">{kind}: </span>{/if}<SourcedValue
    {v}
    {linker}
    {curieIri}
    {prefixes}
    showLang={foreign(v)}
  />&nbsp;<PredicateName {predicates} {prefixes} href={predicateHref} />
{/snippet}

{#if first || more.length}
  <div class="lead">
    {#if first?.values.length}
      <p class="lede">
        {@render value(
          first.values[0],
          first.kind === 'Definition' ? null : first.kind,
          first.predicates,
        )}
      </p>
    {/if}
    {#if first?.translations.length || more.length}
      <div class="more">
        {#if first?.translations.length}
          {@const n = languageCount(first.translations)}
          <details data-more="translations" {open}>
            <summary>In {n} other {n === 1 ? 'language' : 'languages'}</summary>
            <ul>
              {#each first.translations as v, i (i)}
                <li>{@render value(v, null, first.predicates)}</li>
              {/each}
            </ul>
          </details>
        {/if}
        {#each more as d (d.word)}
          <details data-more={d.word} {open}>
            <summary>{d.label}</summary>
            <ul>
              {#each d.rows as g (JSON.stringify([g.kind, g.predicates]))}
                {#each g.values as v, i (i)}
                  <li data-kind={g.kind}>
                    {@render value(v, d.word === 'note' ? g.kind : null, g.predicates)}
                  </li>
                {/each}
              {/each}
            </ul>
          </details>
        {/each}
      </div>
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
  /* The disclosures stack in a browser that cannot split one, and otherwise their summaries share
     a wrapping row and stay in place while what they open falls below the row. */
  .more {
    display: flex;
    flex-wrap: wrap;
    column-gap: var(--sp-4);
    min-width: 0;
  }
  .more details {
    flex: 0 0 100%;
    min-width: 0;
  }
  @supports selector(::details-content) {
    .more details {
      display: contents;
    }
    .more summary {
      order: 0;
    }
    .more details::details-content {
      order: 1;
      flex: 0 0 100%;
      min-width: 0;
    }
  }
  summary {
    cursor: pointer;
    color: var(--text-2);
    font-size: var(--fs-sm);
    font-weight: 500;
  }
  summary::marker {
    color: var(--text-3);
  }
  ul {
    list-style: none;
    margin: var(--sp-2) 0 var(--sp-1);
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
</style>
