<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { cancelTask } from '$lib/backups';
  import { fmtRelative } from '$lib/format';
  import { followTasks, taskActive, taskFeed } from '$lib/tasks.svelte';

  let {
    dataset,
    ondone,
    limit = 12,
    refreshKey = 0,
    filter,
    empty = 'No tasks yet. Compact, backup, reasoning and full-text index jobs show up here.',
  }: {
    dataset?: string;
    /** Called when a task we saw running finishes. */
    ondone?: (t: api.Task) => void;
    limit?: number;
    /** Bump to force an immediate poll (e.g. right after starting a task). */
    refreshKey?: number;
    /** Only the tasks this accepts (e.g. backup kinds). */
    filter?: (t: api.Task) => boolean;
    /** What to say when there are none. */
    empty?: string;
  } = $props();

  let now = $state(Date.now());

  const shown = $derived(
    taskFeed.tasks
      .filter((t) => (!dataset || t.dataset === dataset) && (!filter || filter(t)))
      .slice(0, limit),
  );
  const active = taskActive;
  /** Cancelling needs admin on the task's dataset, or server-admin for server tasks. */
  const mayCancel = (t: api.Task) =>
    t.cancellable === true &&
    active(t) &&
    (t.dataset ? auth.can(t.dataset, 'admin') : auth.hasServer('server-admin'));
  let cancelling = $state<Record<string, boolean>>({});

  async function cancel(t: api.Task) {
    cancelling[t.id] = true;
    try {
      await cancelTask(t.id);
      toasts.push('info', `Cancelling ${t.kind}`, `Task ${t.id}`);
      await taskFeed.refresh();
    } catch (e) {
      toasts.error(`Could not cancel task ${t.id}`, e);
    } finally {
      cancelling[t.id] = false;
    }
  }

  // the list comes from the one poller of /$/tasks (lib/tasks.svelte.ts); a bump of
  // refreshKey loads it now
  let lastKey = untrack(() => refreshKey);
  $effect(() => {
    if (refreshKey === lastKey) return;
    lastKey = refreshKey;
    void taskFeed.refresh();
  });

  onMount(() => {
    const unfollow = followTasks(
      (t) => ondone?.(t),
      (t) => !dataset || t.dataset === dataset,
    );
    // relative times only: no request
    const tick = setInterval(() => (now = Date.now()), 15_000);
    return () => {
      unfollow();
      clearInterval(tick);
    };
  });
</script>

{#if taskFeed.error}
  <div class="error-box">
    <strong>Could not load tasks.</strong> <span class="muted">{taskFeed.error}</span>
  </div>
{:else if taskFeed.loaded && shown.length === 0}
  <p class="faint none">{empty}</p>
{:else}
  <ul class="tasks">
    {#each shown as t (t.id)}
      <li class={t.state}>
        <span class="state" title={t.state}></span>
        <span class="kind" title={t.kind}>{t.kind}</span>
        {#if !dataset}
          <span class="ds mono" title={t.dataset ? t.target : 'Server task'}
            >{t.dataset || t.target || 'server'}</span
          >
        {/if}
        <span class="msg" title={t.message}>
          {#if t.state === 'running'}
            <span
              class="progress"
              role="progressbar"
              aria-label="{t.kind} progress"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={t.progress != null ? Math.round(t.progress * 100) : undefined}
              ><span style:width="{Math.round((t.progress ?? 0) * 100)}%"></span></span
            >
            <span class="faint"
              >{t.progress != null ? `${Math.round(t.progress * 100)}%` : 'running'}</span
            >
            {#if t.message}<span class="detail">{t.message}</span>{/if}
          {:else if t.state === 'queued'}
            <span class="badge">queued</span>
            {#if t.message}<span class="detail">{t.message}</span>{/if}
          {:else}
            {t.message ?? t.state}
          {/if}
        </span>
        {#if mayCancel(t)}
          <button
            class="btn sm"
            onclick={() => cancel(t)}
            disabled={cancelling[t.id]}
            aria-label="Cancel task {t.id} ({t.kind})">Cancel</button
          >
        {/if}
        <span class="when faint" title={t.finishedAt ?? t.startedAt}
          >{fmtRelative(t.finishedAt ?? t.startedAt, now)}</span
        >
      </li>
    {/each}
  </ul>
{/if}

<style>
  .none {
    padding: 4px 0;
    font-size: var(--fs-sm);
  }
  .tasks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
  }
  li {
    display: grid;
    grid-template-columns: 10px 104px auto 1fr auto;
    align-items: center;
    gap: 10px;
    padding: 7px 0;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  li:last-child {
    border-bottom: 0;
  }
  .state {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok);
  }
  .running .state {
    background: var(--spark);
    animation: pulse 1s ease-in-out infinite;
  }
  .failed .state {
    background: var(--danger);
  }
  .failed .msg {
    color: var(--danger);
  }
  .queued .state {
    background: var(--text-3);
    animation: pulse 1.6s ease-in-out infinite;
  }
  .cancelled .state {
    background: var(--text-3);
  }
  .cancelled .msg {
    color: var(--text-3);
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .kind {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .detail {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ds {
    color: var(--text-2);
  }
  .msg {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-2);
  }
  .progress {
    flex: 1;
    max-width: 200px;
    height: 5px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--spark);
    transition: width 0.3s;
  }
  li:has(.ds) {
    grid-template-columns: 10px 104px auto 1fr auto;
  }
  li:not(:has(.ds)) {
    grid-template-columns: 10px 104px 1fr auto;
  }
  li:has(.ds):has(.btn) {
    grid-template-columns: 10px 104px auto 1fr auto auto;
  }
  li:not(:has(.ds)):has(.btn) {
    grid-template-columns: 10px 104px 1fr auto auto;
  }
</style>
