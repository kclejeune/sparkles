<script lang="ts">
  // The field of an Ask thread: the question, Ask or Stop, the run mode, where an answer
  // comes from, and the model that drafts first, as the Ask bar showed them (C18 §6.2).
  import { askThread } from '$lib/ask-thread.svelte';
  import { chipText } from '$lib/ask-thread';
  import { routingLines } from '$lib/ask';
  import Icon from '../Icon.svelte';

  let { compact = false, placeholder }: { compact?: boolean; placeholder: string } = $props();

  let input: HTMLInputElement | undefined = $state();
  $effect(() => {
    const f = () => input?.focus();
    askThread.bindFocus(f);
    return () => askThread.bindFocus(null);
  });

  const busy = $derived(askThread.running);
  const chip = $derived(askThread.chip);
  const lastUsage = $derived([...askThread.thread.turns].reverse().find((t) => t.usage)?.usage);

  function submit(e: SubmitEvent) {
    e.preventDefault();
    const q = askThread.draft.trim();
    if (!q || busy) return;
    askThread.draft = '';
    void askThread.ask(q);
  }
</script>

<div class="composer" class:compact>
  <div class="in">
    {#if chip && askThread.thread.turns.length}
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
    <form onsubmit={submit} aria-label="Ask a question">
      <label class="sr-only" for="ask-thread-input">Question</label>
      <input
        id="ask-thread-input"
        class="input"
        bind:this={input}
        bind:value={askThread.draft}
        maxlength={2000}
        {placeholder}
        autocomplete="off"
        onkeydown={(e) => {
          if (e.key === 'Escape' && compact) {
            e.preventDefault();
            askThread.close();
          }
        }}
      />
      {#if busy}
        <button type="button" class="btn" onclick={() => askThread.stop()}
          ><Icon name="x" size={12} /> Stop</button
        >
      {:else}
        <button type="submit" class="btn primary" disabled={!askThread.draft.trim()}
          ><Icon name="sparkle" size={12} /> Ask</button
        >
      {/if}
    </form>
    <div class="opts">
      <span class="segmented" role="radiogroup" aria-label="Run mode">
        <button
          type="button"
          role="radio"
          aria-checked={askThread.mode === 'preview'}
          onclick={() => askThread.setMode('preview')}
          >{compact ? 'Preview' : 'Preview first'}</button
        >
        <button
          type="button"
          role="radio"
          aria-checked={askThread.mode === 'run'}
          onclick={() => askThread.setMode('run')}>{compact ? 'Run' : 'Run, then show'}</button
        >
      </span>
      <span class="segmented" role="radiogroup" aria-label="Answer from">
        {#each [['auto', 'Auto'], ['local', 'Data'], ['global', 'Summaries']] as const as [v, l] (v)}
          <button
            type="button"
            role="radio"
            aria-checked={askThread.source === v}
            onclick={() => (askThread.source = v)}>{l}</button
          >
        {/each}
      </span>
      {#if askThread.model}
        <span
          class="model"
          title={['The model that drafts first', ...routingLines(lastUsage)].join('\n')}
          >{askThread.model}</span
        >
      {/if}
    </div>
  </div>
</div>

<style>
  .composer {
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .in {
    max-width: 820px;
    margin: 0 auto;
    padding: var(--sp-3) var(--gutter);
    display: grid;
    gap: var(--sp-2);
  }
  .compact .in {
    padding: var(--sp-3) var(--sp-4);
  }
  form {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
  }
  .input {
    flex: 1;
    height: 36px;
    font-size: var(--fs-md);
  }
  form .btn {
    height: 36px;
    padding: 0 var(--sp-4);
  }
  .compact .input,
  .compact form .btn {
    height: var(--ctl-h);
    font-size: var(--fs);
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    font-size: var(--fs-sm);
  }
  .model {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
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
    overflow: hidden;
    white-space: nowrap;
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
    flex: none;
  }
  .chip button:hover {
    background: var(--hover);
  }
  .chip button:focus-visible {
    outline: none;
    box-shadow: var(--focus);
  }
  @media (max-width: 760px) {
    .input,
    form .btn,
    .compact .input,
    .compact form .btn {
      height: 44px;
    }
  }
</style>
