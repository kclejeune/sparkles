<script lang="ts">
  import type { Term } from '$lib/api';
  import TermView from './TermView.svelte';
  import { displayIri, shorten, XSD_STRING, type PrefixMap } from '$lib/rdf';

  let {
    term,
    prefixes,
    onopen,
    full = false,
  }: {
    term: Term | null | undefined;
    prefixes: PrefixMap;
    onopen?: (iri: string) => void;
    /** Show the full IRI instead of the prefixed form. */
    full?: boolean;
  } = $props();

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
      {full ? term.value : displayIri(term.value, prefixes)}
    </button>
  {:else}
    <span class="t-iri" title={term.value}
      >{full ? term.value : displayIri(term.value, prefixes)}</span
    >
  {/if}
{:else if term.type === 'bnode'}
  <span class="t-bnode">_:{term.value}</span>
{:else if term.type === 'literal'}
  <span class="t-literal lit">{term.value}</span>{#if term['xml:lang']}<span class="meta"
      >@{term['xml:lang']}</span
    >{:else if dt}<span class="meta" title={term.datatype}>^^{dt}</span>{/if}
{:else}
  <span class="quoted"
    >«<TermView term={term.value.subject} {prefixes} {onopen} />
    <TermView term={term.value.predicate} {prefixes} {onopen} />
    <TermView term={term.value.object} {prefixes} {onopen} />»</span
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
  .link:focus-visible {
    box-shadow: var(--focus);
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
