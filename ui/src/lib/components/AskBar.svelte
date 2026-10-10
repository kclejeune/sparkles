<script lang="ts">
  // The Ask bar of the query page (C18 §6.2): a question, the run mode, the model that
  // drafts, and while the server works the step it is on with Stop. A question asked in a
  // tab with an answer is a follow-up, and New question starts without the earlier turns.
  import type { RunMode, Turn } from '$lib/ask';
  import Icon from './Icon.svelte';

  let {
    value = $bindable(''),
    mode = $bindable<RunMode>('preview'),
    model,
    step,
    context,
    onask,
    onstop,
    onnew,
  }: {
    value: string;
    mode: RunMode;
    /** The first draft pair, such as `local · qwen3:8b`. */
    model: string | null;
    /** The step the server is on, or null when idle. */
    step: string | null;
    /** The earlier turns a question asked now carries. */
    context: Turn[];
    onask: () => void;
    onstop: () => void;
    onnew: () => void;
  } = $props();

  let input: HTMLInputElement | undefined = $state();
  export const focus = () => input?.focus();
  const busy = $derived(step != null);
</script>

<section class="askbar" aria-label="Ask bar">
  <form
    class="row1"
    onsubmit={(e) => {
      e.preventDefault();
      if (value.trim() && !busy) onask();
    }}
  >
    <label class="lbl" for="ask-input">Ask</label>
    <input
      id="ask-input"
      class="input"
      bind:this={input}
      bind:value
      maxlength={2000}
      placeholder={context.length
        ? 'Ask a follow-up question…'
        : 'Ask a question about this dataset…'}
      autocomplete="off"
    />
    {#if busy}
      <button type="button" class="btn sm" onclick={onstop}><Icon name="x" size={12} /> Stop</button
      >
    {:else}
      <button type="submit" class="btn primary sm" disabled={!value.trim()}>
        <Icon name="sparkle" size={12} /> Ask
      </button>
    {/if}
  </form>
  <div class="row2 small">
    <div class="modes" role="radiogroup" aria-label="Run mode">
      <span class="faint">Mode</span>
      <label
        ><input type="radio" name="ask-mode" value="preview" bind:group={mode} /> Preview first</label
      >
      <label
        ><input type="radio" name="ask-mode" value="run" bind:group={mode} /> Run, then show</label
      >
    </div>
    {#if context.length}
      <span class="followup faint" title={context.map((t) => t.question).join('\n')}>
        Follow-up of “{context[context.length - 1].question}”
      </span>
      <button type="button" class="btn ghost sm" onclick={onnew}>New question</button>
    {/if}
    <span class="spacer"></span>
    {#if busy}
      <span class="step" role="status" aria-live="polite"
        ><span class="spinner"></span> {step}…</span
      >
    {:else if model}
      <span class="faint mono model" title="The model that drafts first">{model}</span>
    {/if}
  </div>
</section>

<style>
  .askbar {
    display: grid;
    gap: 4px;
    padding: 8px 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .row1 {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .lbl {
    font-weight: 600;
    font-size: var(--fs-sm);
  }
  .row1 .input {
    flex: 1;
    min-width: 0;
  }
  .row2 {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px 12px;
    min-width: 0;
  }
  .modes {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .modes label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .followup {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40ch;
  }
  .spacer {
    flex: 1;
  }
  .step {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .model {
    font-size: var(--fs-xs);
  }
</style>
