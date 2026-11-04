<script lang="ts">
  // The predicates that state a text, as small chips at its end: each as the Full/Prefixed switch
  // writes it, with its full IRI on hover and a link to its page when it has one (ROADMAP item
  // 633). A chip stays on one line unless it is a full IRI too long to fit a phone.
  import { displayIri, type PrefixMap } from '$lib/rdf';

  let {
    predicates,
    prefixes,
    href,
  }: {
    predicates: string[];
    prefixes: PrefixMap;
    href?: (iri: string) => string | null;
  } = $props();
  const LONG = 32;
</script>

{#each predicates as p, i (p)}{#if i > 0}{' '}{/if}{@const shown = displayIri(
    p,
    prefixes,
  )}{#if href?.(p)}<a class="pred" class:long={shown.length > LONG} href={href(p)} title={p}
      >{shown}</a
    >{:else}<span class="pred" class:long={shown.length > LONG} title={p}>{shown}</span>{/if}{/each}

<style>
  .pred {
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    color: var(--text-3);
    font: 400 11px var(--font-mono);
    white-space: nowrap;
    vertical-align: 1px;
    text-decoration: none;
    -webkit-box-decoration-break: clone;
    box-decoration-break: clone;
  }
  .pred.long {
    white-space: normal;
    overflow-wrap: anywhere;
  }
  a.pred:hover {
    color: var(--iri);
    text-decoration: underline;
  }
</style>
