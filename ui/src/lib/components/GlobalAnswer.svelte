<script lang="ts">
  // A global answer (spec C22 §2.4): the answer written from community reports, its [n]
  // markers linked to the points it cites, the points with their scores, and the reports
  // they come from with the findings used and the facts each finding cites. The answer is
  // labelled as generated from summaries, and **Answer from the data** asks again with a
  // query.
  import { resolve } from '$app/paths';
  import type { AskGlobal, AskSummary } from '$lib/ask-api';
  import { summaryParts } from '$lib/ask';
  import { fmtInt } from '$lib/format';
  import { citeText } from '$lib/graphrag';
  import Icon from './Icon.svelte';

  let {
    ds,
    answer,
    summary,
    busy = false,
    onlocal,
  }: {
    ds: string;
    answer: AskGlobal;
    summary?: AskSummary;
    busy?: boolean;
    /** Ask the same question again, answered from the data. */
    onlocal?: () => void;
  } = $props();

  let lit = $state<number | null>(null);
  const parts = $derived(summary ? summaryParts(summary.text, summary.citations) : []);
  const cited = $derived(new Set(summary?.citations ?? []));
  const model = $derived(summary?.model ? `${summary.provider ?? ''} · ${summary.model}` : '');
  const titleOf = $derived(new Map(answer.reports.map((r) => [r.iri, r.title])));
  const communitiesHref = $derived(`${resolve('/datasets/[name]', { name: ds })}?tab=communities`);

  function show(n: number) {
    lit = n;
    document.getElementById(`gpoint-${n}`)?.scrollIntoView({ block: 'nearest' });
  }
</script>

<section class="global" aria-label="Answer from community reports" data-testid="global-answer">
  <div class="head">
    <span class="key">Answer</span>
    <p class="text">
      {#if summary}
        {#each parts as p, i (i)}{#if p.rows}<button
              class="cite"
              class:lit={lit != null && p.rows.includes(lit)}
              title="Point {p.rows.join(', ')}"
              onclick={() => show(p.rows![0])}>{p.text}</button
            >{:else}{p.text}{/if}{/each}
      {:else}
        <span class="faint">Writing the answer…</span>
      {/if}
    </p>
  </div>
  <div class="meta faint small">
    <span
      >generated from community summaries · {fmtInt(answer.reports.length)} report{answer.reports
        .length === 1
        ? ''
        : 's'}{#if answer.global.level != null}, level {answer.global.level}{/if}</span
    >
    {#if summary}<span>cites {fmtInt(cited.size)} of {fmtInt(answer.points.length)} points</span
      >{/if}
    {#if model}<span class="mono" title="The model that wrote the answer">{model}</span>{/if}
    {#if onlocal}
      <button
        class="btn sm"
        disabled={busy}
        title="Ask again and answer with a query over the data"
        onclick={onlocal}><Icon name="query" size={12} /> Answer from the data</button
      >
    {/if}
  </div>
  {#if summary?.uncited}
    <p class="warn small">The answer cites no point, so check it against the reports below.</p>
  {:else if summary?.uncitedSentences?.length}
    <p class="warn small">
      {fmtInt(summary.uncitedSentences.length)} sentence{summary.uncitedSentences.length === 1
        ? ''
        : 's'} cite no point.
    </p>
  {/if}

  {#if answer.points.length}
    <h3 class="sub">Points</h3>
    <ol class="points">
      {#each answer.points as p (p.n)}
        <li id="gpoint-{p.n}" class:lit={lit === p.n} class:uncited={!cited.has(p.n)}>
          <span class="n mono">[{p.n}]</span>
          <span class="ptext">{p.text}</span>
          <span class="faint small"
            >score {p.score} · {p.reports.map((r) => titleOf.get(r) ?? r).join(', ')}</span
          >
        </li>
      {/each}
    </ol>
  {/if}

  {#if answer.reports.length}
    <h3 class="sub">
      Reports <a class="small" href={communitiesHref}>Open the Communities tab</a>
    </h3>
    <ul class="reports">
      {#each answer.reports as r (r.iri)}
        <li>
          <details>
            <summary>
              <span class="rtitle">{r.title}</span>
              <span class="badge">{r.status}</span>
              <span class="faint small">level {r.level}</span>
            </summary>
            {#each r.findings as f (f.iri)}
              <div class="finding">
                <p>{f.text}</p>
                {#if f.cites.length}
                  <ul class="cites mono small">
                    {#each f.cites as c (c)}<li>{citeText(c)}</li>{/each}
                  </ul>
                {/if}
              </div>
            {:else}
              <p class="faint small">The answer uses this report's summary, not one finding.</p>
            {/each}
          </details>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .global {
    display: grid;
    gap: 6px;
    padding: 8px 12px 12px;
    background: color-mix(in srgb, var(--iri) 5%, var(--surface));
    font-size: var(--fs-sm);
    min-width: 0;
    overflow-y: auto;
  }
  .head {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    min-width: 0;
  }
  .key {
    flex: none;
    font-weight: 600;
  }
  .text {
    flex: 1;
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .cite {
    all: unset;
    cursor: pointer;
    color: var(--iri);
    font-weight: 600;
    border-radius: 3px;
    padding: 0 1px;
  }
  .cite:hover,
  .cite.lit {
    background: color-mix(in srgb, var(--iri) 18%, transparent);
  }
  .cite:focus-visible {
    box-shadow: 0 0 0 2px var(--iri);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 12px;
  }
  .warn {
    margin: 0;
    color: var(--warn, var(--muted));
  }
  .sub {
    margin: 6px 0 0;
    font-size: var(--fs-sm);
    display: flex;
    gap: 8px;
    align-items: baseline;
    flex-wrap: wrap;
  }
  .points,
  .reports {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .points li {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0 6px;
    padding: 4px 6px;
    border-radius: 4px;
    min-width: 0;
  }
  .points li .faint {
    grid-column: 2;
    overflow-wrap: anywhere;
  }
  .points li.lit {
    background: color-mix(in srgb, var(--iri) 14%, transparent);
  }
  .points li.uncited .n {
    opacity: 0.6;
  }
  .ptext {
    overflow-wrap: anywhere;
  }
  .reports summary {
    cursor: pointer;
    display: flex;
    gap: 8px;
    align-items: baseline;
    flex-wrap: wrap;
  }
  .rtitle {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .finding {
    padding: 4px 0 4px 14px;
    min-width: 0;
  }
  .finding p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .cites {
    margin: 2px 0 0;
    padding-left: 14px;
    overflow-wrap: anywhere;
  }
</style>
