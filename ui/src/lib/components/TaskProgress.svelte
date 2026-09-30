<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { cancelTask, followTask, isActive } from '$lib/backups';

  let {
    task,
    onfinish,
  }: {
    /** The task as the request that started it returned it. */
    task: api.Task;
    /** Called once when the task is no longer queued or running. */
    onfinish?: (t: api.Task) => void;
  } = $props();

  let current = $state<api.Task | null>(null);
  let error = $state<string | null>(null);
  let cancelling = $state(false);
  const t = $derived(current ?? task);
  const pct = $derived(t.progress != null ? Math.round(t.progress * 100) : null);

  onMount(() => {
    const ctl = new AbortController();
    followTask(task.id, (x) => (current = x), { signal: ctl.signal }).then(
      (x) => onfinish?.(x),
      (e) => {
        if (!(e instanceof DOMException && e.name === 'AbortError')) error = api.errorMessage(e);
      },
    );
    return () => ctl.abort();
  });

  async function cancel() {
    cancelling = true;
    try {
      await cancelTask(t.id);
    } catch (e) {
      toasts.error('Could not cancel the task', e);
    } finally {
      cancelling = false;
    }
  }
</script>

<div class="task {t.state}" aria-live="polite">
  <div class="row">
    <span class="state">
      {#if isActive(t)}<span class="spinner"></span>{/if}
      {t.state === 'queued'
        ? 'Queued'
        : t.state === 'running'
          ? 'Running'
          : t.state === 'done'
            ? 'Done'
            : t.state === 'cancelled'
              ? 'Cancelled'
              : 'Failed'}
    </span>
    <span class="faint mono small">task {t.id}</span>
    <span class="spacer"></span>
    {#if isActive(t) && t.cancellable}
      <button class="btn sm" onclick={cancel} disabled={cancelling}>Cancel</button>
    {/if}
  </div>
  {#if t.state === 'running'}
    <div
      class="bar"
      role="progressbar"
      aria-label="Progress"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct ?? undefined}
    >
      <span style:width="{pct ?? 0}%"></span>
    </div>
  {/if}
  {#if t.message}<p class="msg">{t.message}</p>{/if}
  {#if error}<p class="msg bad">Lost track of the task: {error}</p>{/if}
</div>

<style>
  .task {
    display: grid;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
  }
  .done .state {
    color: var(--ok);
  }
  .failed .state,
  .bad {
    color: var(--danger);
  }
  .cancelled .state {
    color: var(--text-2);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--spark);
    transition: width 0.3s;
  }
  .msg {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
</style>
