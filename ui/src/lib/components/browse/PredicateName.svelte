<script lang="ts">
  // The predicates the page names beside a text: each as the Full/Prefixed switch
  // writes it, muted, with its full IRI on hover and a link to its page when it has one
  // (ROADMAP item 633). `dot` splits them from a kind written on the same line.
  import { displayIri, type PrefixMap } from '$lib/rdf';

  let {
    predicates,
    prefixes,
    href,
    dot = true,
  }: {
    predicates: string[];
    prefixes: PrefixMap;
    href?: (iri: string) => string | null;
    dot?: boolean;
  } = $props();
</script>

<span class="preds" class:bare={!dot}
  >{#if dot}<span class="dot" aria-hidden="true">·</span
    >{/if}{#each predicates as p, i (p)}{#if i > 0},{/if}{' '}{#if href?.(p)}<a
        href={href(p)}
        title={p}>{displayIri(p, prefixes)}</a
      >{:else}<span title={p}>{displayIri(p, prefixes)}</span>{/if}{/each}</span
>

<style>
  .preds {
    margin-left: 0.4em;
  }
  .preds.bare {
    margin-left: 0;
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
