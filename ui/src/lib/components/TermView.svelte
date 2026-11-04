<script lang="ts">
  import type { Term } from '$lib/api';
  import TermView from './TermView.svelte';
  import {
    abbreviateVector,
    displayEntity,
    isVectorLiteral,
    shorten,
    XSD_STRING,
    type PrefixMap,
  } from '$lib/rdf';

  let {
    term,
    prefixes,
    onopen,
    full = false,
    expandable = false,
    labels,
  }: {
    term: Term | null | undefined;
    labels?: ReadonlyMap<string, import('$lib/ontology').Lit[]>;
    prefixes: PrefixMap;
    onopen?: (iri: string) => void;
    /** Show the full IRI instead of the prefixed form. */
    full?: boolean;
    /** Vector literals can be clicked to show every component (else: in the tooltip). */
    expandable?: boolean;
  } = $props();

  const vector = $derived(isVectorLiteral(term));
  let expanded = $state(false);
  const dt = $derived(
    term?.type === 'literal' && term.datatype && term.datatype !== XSD_STRING
      ? (shorten(term.datatype, prefixes) ?? term.datatype)
      : null,
  );
</script>

{#if !term}
  <span class="unbound">—</span>
{:else if term.type === 'uri'}
  {#if onopen}
    <button class="link t-iri" title={term.value} onclick={() => onopen(term.value)}>
      {full ? term.value : displayEntity(term.value, prefixes, labels?.get(term.value))}
    </button>
  {:else}
    <span class="t-iri" title={term.value}
      >{full ? term.value : displayEntity(term.value, prefixes, labels?.get(term.value))}</span
    >
  {/if}
{:else if term.type === 'bnode'}
  <span class="t-bnode">_:{term.value}</span>
{:else if term.type === 'literal' && vector}
  {#if expandable}
    <button
      class="vec t-literal"
      class:open={expanded}
      title={expanded ? 'Collapse' : term.value}
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
      >{expanded ? term.value : abbreviateVector(term.value, 3, term.datatype)}</button
    >
  {:else}
    <span class="t-literal lit" title={term.value}
      >{abbreviateVector(term.value, 3, term.datatype)}</span
    >
  {/if}
{:else if term.type === 'literal'}
  <span class="t-literal lit">{term.value}</span>{#if term['xml:lang']}<span class="meta"
      >@{term['xml:lang']}</span
    >{:else if dt}<span class="meta" title={term.datatype}>^^{dt}</span>{/if}
{:else}
  <span class="quoted"
    >«<TermView term={term.value.subject} {prefixes} {onopen} {labels} {full} />
    <TermView term={term.value.predicate} {prefixes} {onopen} {labels} {full} />
    <TermView term={term.value.object} {prefixes} {onopen} {labels} {full} />»</span
  >
{/if}

<style>
  .link {
    all: unset;
    color: var(--iri);
    cursor: pointer;
    border-radius: 2px;
  }
  .link:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .link:focus-visible,
  .vec:focus-visible {
    box-shadow: var(--focus);
  }
  .vec {
    all: unset;
    cursor: pointer;
    border-radius: 2px;
    border-bottom: 1px dotted currentColor;
  }
  .vec.open {
    border-bottom: 0;
    word-break: break-all;
  }
  .meta {
    color: var(--text-3);
    font-size: 0.88em;
    margin-left: 3px;
  }
  .unbound {
    color: var(--text-3);
  }
  .quoted {
    color: var(--text-2);
  }
</style>
