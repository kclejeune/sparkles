<script lang="ts">
  // The labels of a SKOS resource as rows by kind, in the style of the synonyms: Preferred and
  // Alternative, with every value in every language and its language tag, and the hidden labels
  // as a row of their own behind a toggle.
  import { labelKinds, type Sections } from '$lib/browse';

  let { labels }: { labels: Sections['labels'] } = $props();
  let hidden = $state(false);
  const hiddenCount = $derived(labels.reduce((n, r) => n + r.hidden.length, 0));
  const rows = $derived(labelKinds(labels).filter((g) => hidden || g.kind !== 'Hidden'));
  const rtl = (lang: string) => /^(ar|he|fa|ur|yi|ps|sd|ug|dv)(-|$)/i.test(lang);
</script>

{#if labels.length}
  <section class="term-sec" aria-labelledby="labels-h">
    <h2 id="labels-h">Labels</h2>
    <dl class="term-rows">
      {#each rows as g (g.kind)}
        <div class="term-row">
          <dt>{g.kind}</dt>
          <dd class="values">
            {#each g.values as v (v.lang + v.text)}
              <span data-lang={v.lang}
                ><span dir={rtl(v.lang) ? 'rtl' : undefined}>{v.text}</span>{#if v.lang}<span
                    class="lang-tag">{v.lang}</span
                  >{/if}</span
              >
            {/each}
          </dd>
        </div>
      {/each}
    </dl>
    {#if hiddenCount}
      <p class="faint hint">
        {#if hidden}Hidden labels are shown.{:else}{hiddenCount} hidden {hiddenCount === 1
            ? 'label is'
            : 'labels are'} not shown.{/if}
        <button class="more-btn" type="button" onclick={() => (hidden = !hidden)}
          >{hidden ? 'Hide them' : 'Show them'}</button
        >
      </p>
    {/if}
  </section>
{/if}

<style>
  .hint {
    margin-top: 6px;
    font-size: var(--fs-sm);
  }
</style>
