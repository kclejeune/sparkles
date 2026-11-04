<script lang="ts">
  // The labels of a SKOS resource as rows by kind, in the style of the synonyms: Preferred,
  // Alternative and Hidden, with every value in every language and its language tag.
  import { labelKinds, type Sections } from '$lib/browse';

  let { labels }: { labels: Sections['labels'] } = $props();
  const rows = $derived(labelKinds(labels));
  const rtl = (lang: string) => /^(ar|he|fa|ur|yi|ps|sd|ug|dv)(-|$)/i.test(lang);
</script>

{#if rows.length}
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
  </section>
{/if}
