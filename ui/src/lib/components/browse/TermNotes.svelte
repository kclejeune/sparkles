<script lang="ts">
  // Comments and notes: `rdfs:comment` and the SKOS notes, in the page's best language,
  // with the others behind "Other languages". CURIEs in the text link to their pages.
  import { langRank, type Linker, type Sections } from '$lib/browse';
  import SourcedValue from './SourcedValue.svelte';

  let {
    notes,
    linker,
    langs,
    curieIri,
  }: {
    notes: Sections['notes'];
    linker: Linker;
    langs: readonly string[];
    curieIri?: (curie: string) => string | null;
  } = $props();

  /** Each kind's notes in the best language present, and the rest. */
  const split = $derived(
    notes.map((g) => {
      const best = Math.min(...g.values.map((v) => langRank(v.lang, langs)));
      return {
        kind: g.kind,
        shown: g.values.filter((v) => langRank(v.lang, langs) === best),
        other: g.values.filter((v) => langRank(v.lang, langs) !== best),
      };
    }),
  );
</script>

{#each split as g (g.kind)}
  <section class="term-sec" aria-label={g.kind}>
    <h2>
      {g.kind}{#if g.shown.length > 1}s{/if}
    </h2>
    {#each g.shown as v, i (i)}
      <p class="note"><SourcedValue {v} {linker} {curieIri} showLang={false} /></p>
    {/each}
    {#if g.other.length}
      <details class="others">
        <summary>Other languages <span class="count-badge">{g.other.length}</span></summary>
        {#each g.other as v, i (i)}
          <p class="note"><SourcedValue {v} {linker} {curieIri} /></p>
        {/each}
      </details>
    {/if}
  </section>
{/each}

<style>
  .note {
    max-width: 72ch;
    color: var(--text-2);
    line-height: 1.55;
    border-left: 2px solid var(--border-strong);
    padding-left: var(--sp-3);
    margin: 0 0 var(--sp-2);
  }
  .others summary {
    cursor: pointer;
    color: var(--text-2);
    font-weight: 500;
    font-size: var(--fs-sm);
    margin-bottom: var(--sp-2);
  }
</style>
