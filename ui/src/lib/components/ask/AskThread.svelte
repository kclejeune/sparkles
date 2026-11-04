<script lang="ts">
  // The Ask thread (C18 §6.2) as the page at /ui/ask and the drawer show it: the turns,
  // newest last, then the field. An empty thread offers the page's context as a chip and
  // questions that fit it. The actions hand a turn to Query or Explore.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { app, toasts } from '$lib/app.svelte';
  import { askThread } from '$lib/ask-thread.svelte';
  import { chipSuggestions, chipText, exploreTarget, type ThreadTurn } from '$lib/ask-thread';
  import { questionTitle, type TabQuestion } from '$lib/ask';
  import type { Feedback } from '$lib/ask-api';
  import { tick } from 'svelte';
  import Icon from '../Icon.svelte';
  import AskTurn from './AskTurn.svelte';
  import AskComposer from './AskComposer.svelte';

  let { compact = false }: { compact?: boolean } = $props();

  const ds = $derived(askThread.ds ?? '');
  const prefixes = $derived(app.prefixes(ds));
  const turns = $derived(askThread.thread.turns);
  const chip = $derived(askThread.chip);
  const suggestions = $derived(chipSuggestions(chip));
  let scroller: HTMLElement | undefined = $state();

  // a new turn scrolls into view
  let seen = 0;
  $effect(() => {
    const n = turns.length;
    if (n > seen && scroller) {
      void tick().then(() => {
        const last = scroller?.querySelector('.turn:last-of-type');
        last?.scrollIntoView({ block: 'start', behavior: 'auto' });
      });
    }
    seen = n;
  });

  /** The tab question a turn opens in Query with, so its header and summary work there. */
  export function tabQuestion(t: ThreadTurn): TabQuestion {
    return {
      question: t.question,
      explanation: t.result?.explanation,
      assumptions: t.result?.assumptions,
      original: t.result?.query ?? '',
      ...(t.id ? { askId: t.id } : {}),
      ...(t.summary ? { summary: t.summary } : {}),
      ...(t.usage ? { usage: t.usage } : {}),
      ...(t.mode ? { mode: t.mode } : {}),
      ...(t.global ? { global: t.global } : {}),
      graph: t.result?.graph ?? null,
      ...(t.feedback ? { feedback: t.feedback } : {}),
    };
  }

  function openInQuery(t: ThreadTurn, save = false) {
    const query = t.result?.query;
    if (!query) return;
    const question = tabQuestion(t);
    if (askThread.openInQuery({ query, question, save, result: t.result?.results })) return;
    app.pendingQuery = {
      query,
      title: questionTitle(t.question),
      question,
      ...(save ? { saveExample: true } : {}),
    };
    if (!compact) askThread.open = false;
    void goto(resolve('/query'));
  }

  function openInExplore(t: ThreadTurn) {
    const iri = exploreTarget(t);
    const q = new URLSearchParams({ ds });
    if (iri) q.set('iri', iri);
    void goto(`${resolve('/explore')}?${q}`);
  }

  async function feedback(t: ThreadTurn, f: Feedback) {
    try {
      await askThread.feedback(t, f);
    } catch (e) {
      toasts.error('Could not send the feedback', e);
    }
  }
</script>

<div class="thread" class:compact>
  <div class="turns" bind:this={scroller}>
    <div class="turns-in">
      {#if !turns.length}
        <div class="empty">
          {#if chip}
            <div class="chips">
              <span class="chip"
                >{chipText(chip)}
                <button
                  type="button"
                  aria-label="Remove the context {chipText(chip)}"
                  onclick={() => (askThread.chipRemoved = true)}><Icon name="x" size={11} /></button
                ></span
              >
            </div>
          {/if}
          <p>
            {#if chip?.kind === 'entity'}Ask about this {chip.type ?? 'resource'}, or pick a
              question.
            {:else if chip?.kind === 'query'}Ask about the query in this tab, or pick a question.
            {:else if chip?.kind === 'commit'}Questions read the data as of this commit.
            {:else}Ask a question about {ds || 'this dataset'}. Each answer shows the query it ran,
              its rows and its sources.{/if}
          </p>
          {#if suggestions.length}
            <div class="sugg" role="group" aria-label="Suggested questions">
              {#each suggestions as s (s)}
                <button type="button" class="btn sm" onclick={() => void askThread.ask(s)}
                  >{s}</button
                >
              {/each}
            </div>
          {/if}
        </div>
      {/if}
      {#each turns as t (t.key)}
        <AskTurn
          turn={t}
          {ds}
          {prefixes}
          {compact}
          busy={askThread.running}
          onquery={(x) => openInQuery(x)}
          onexplore={openInExplore}
          onsave={(x) => openInQuery(x, true)}
          onfeedback={feedback}
          onclarify={(x, value) =>
            void askThread.ask(x.question, {
              retry: x,
              clarification: { id: x.clarify?.id ?? '', value },
            })}
          ontryharder={(x) => void askThread.ask(x.question, { retry: x, tryHarder: x.id })}
          onsource={(x, mode) => void askThread.ask(x.question, { retry: x, mode })}
          onrerun={(x) => askThread.rerun(x)}
        />
      {/each}
    </div>
  </div>
  <AskComposer
    {compact}
    placeholder={turns.length
      ? 'Ask a follow-up'
      : chip?.kind === 'entity'
        ? `Ask about ${chip.label}`
        : 'Ask a question about this dataset'}
  />
</div>

<style>
  .thread {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto;
    min-height: 0;
    min-width: 0;
    height: 100%;
  }
  .turns {
    overflow: auto;
    min-height: 0;
  }
  .turns-in {
    max-width: 820px;
    margin: 0 auto;
    padding: var(--sp-5) var(--gutter) var(--sp-6);
    display: grid;
    min-width: 0;
  }
  .compact .turns-in {
    padding: var(--sp-4);
  }
  .compact :global(.turn) {
    padding: var(--sp-4) 0;
  }
  .compact :global(.turn h3) {
    font-size: var(--fs-md);
  }
  .compact :global(.turn .answer),
  .compact :global(.turn .points) {
    font-size: var(--fs);
  }
  .turns-in > :global(.turn:first-of-type) {
    padding-top: 0;
  }
  .empty {
    display: grid;
    gap: var(--sp-3);
    padding-bottom: var(--sp-4);
  }
  .empty + :global(.turn) {
    border-top: 1px solid var(--border);
    padding-top: var(--sp-4);
  }
  .empty p {
    margin: 0;
    color: var(--text-2);
    max-width: 60ch;
  }
  .sugg,
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    max-width: 100%;
    padding: 0 4px 0 8px;
    border-radius: var(--r-pill);
    background: var(--spark-soft);
    color: var(--spark-ink);
    font-size: var(--fs-sm);
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
  }
  .chip button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: none;
    color: inherit;
    cursor: pointer;
  }
  .chip button:focus-visible {
    outline: none;
    box-shadow: var(--focus);
  }
</style>
