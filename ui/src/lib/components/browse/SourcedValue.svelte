<script lang="ts">
  // One annotation value with its language and the sources its axiom annotations name,
  // such as a GO synonym with `GOC:al`.
  import { isLiteral, valueText, type Linker, type Sourced } from '$lib/browse';
  import LinkedText from './LinkedText.svelte';

  let {
    v,
    linker,
    curieIri,
    showLang = true,
  }: {
    v: Sourced;
    linker: Linker;
    curieIri?: (curie: string) => string | null;
    showLang?: boolean;
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
    >{/if}{#if v.sources.length}<span class="src">{v.sources.join(', ')}</span>{/if}</span
>

<style>
  .val {
    overflow-wrap: anywhere;
  }
  .val a {
    color: var(--iri);
  }
  .src {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin-left: 6px;
  }
</style>
