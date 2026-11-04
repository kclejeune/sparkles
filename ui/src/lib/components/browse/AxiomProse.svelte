<script lang="ts">
  // One class expression, property expression, individual or literal of the JSON form as
  // Manchester prose, with every entity a link to its page. The words come from
  // `manchesterTokens` (lib/ontology.ts), which `manchester()` joins, so the prose and the
  // editor's text cannot drift apart.
  import type { Linker } from '$lib/browse';
  import { manchesterTokens } from '$lib/ontology';
  import DatatypeChip from './DatatypeChip.svelte';
  import type { PrefixMap } from '$lib/rdf';

  let { value, linker, prefixes }: { value: unknown; linker: Linker; prefixes: PrefixMap } =
    $props();
  const tokens = $derived(manchesterTokens(value, linker.name, prefixes));
</script>

<span class="prose"
  >{#each tokens as t, i (i)}{#if t.role === 'name' && t.iri}{@const href = linker.href(
        t.iri,
      )}{#if href}<a {href} title={t.iri}>{linker.name(t.iri)}</a>{:else}<span
          class="builtin"
          title={t.iri}>{linker.name(t.iri)}</span
        >{/if}{:else if t.role === 'kw'}<span class="kw">{t.text}</span
      >{:else if t.role === 'lit'}<span class="lit">{t.value ?? t.text}</span><DatatypeChip
        iri={t.datatype ?? null}
        {prefixes}
      />{:else}{t.text}{/if}{/each}</span
>

<style>
  .prose {
    overflow-wrap: anywhere;
  }
  .prose a {
    color: var(--iri);
  }
  .kw {
    color: var(--kw);
    font-weight: 600;
    font-style: italic;
  }
  .lit {
    color: var(--lit, var(--text-2));
  }
  .builtin {
    color: var(--text-2);
  }
</style>
