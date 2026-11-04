<script lang="ts">
  // One annotation value with its language and the sources its axiom annotations name,
  // such as a GO synonym with `GOC:al`.
  import { isLiteral, valueText, type Linker, type Sourced } from '$lib/browse';
  import { localName } from '$lib/rdf';
  import LinkedText from './LinkedText.svelte';

  let {
    v,
    linker,
    curieIri,
    showLang = true,
    typeName = localName,
  }: {
    v: Sourced;
    linker: Linker;
    curieIri?: (curie: string) => string | null;
    showLang?: boolean;
    /** What a synonym type is called, after the value as a chip. */
    typeName?: (iri: string) => string;
  } = $props();
  const iriHref = $derived(isLiteral(v.value) ? null : linker.href(v.value as string));
</script>

<span class="val"
  >{#if !isLiteral(v.value)}{#if iriHref}<a href={iriHref}>{linker.name(v.value as string)}</a
      >{:else}<span class="mono">{v.value}</span>{/if}{:else}<LinkedText
      text={valueText(v.value)}
      {linker}
      {curieIri}
    />{/if}{#if showLang && v.lang}<span class="lang-tag">{v.lang}</span
    >{/if}{#each v.types ?? [] as t (t)}<span class="badge syn-type" title={t}>{typeName(t)}</span
    >{/each}{#if v.sources.length}<span class="src">{v.sources.join(', ')}</span>{/if}</span
>

<style>
  .val {
    overflow-wrap: anywhere;
  }
  .val a {
    color: var(--iri);
  }
  .syn-type {
    margin-left: 6px;
    vertical-align: 1px;
  }
  .src {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin-left: 6px;
  }
</style>
