<script lang="ts">
  // The predicates a heading of the Definition card names, after its kind: each as the
  // Full/Prefixed switch writes it, muted, with its full IRI on hover and a link to its page
  // when it has one (ROADMAP item 633).
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
</script>

<span class="preds"
  ><span class="dot" aria-hidden="true">·</span
  >{#each predicates as p, i (p)}{#if i > 0},{/if}{' '}{#if href?.(p)}<a href={href(p)} title={p}
        >{displayIri(p, prefixes)}</a
      >{:else}<span title={p}>{displayIri(p, prefixes)}</span>{/if}{/each}</span
>

<style>
  .preds {
    margin-left: 0.4em;
  }
  /* a section heading spaces its parts with its own gap */
  :global(.term-sec > h2) > .preds {
    margin-left: calc(0.4em - var(--sp-2));
  }
  .preds {
    font-weight: 400;
    color: var(--text-3);
    overflow-wrap: anywhere;
  }
  .dot {
    margin-right: 0.3em;
  }
  .preds a {
    color: inherit;
    text-decoration: none;
  }
  .preds a:hover {
    color: var(--iri);
    text-decoration: underline;
  }
</style>
