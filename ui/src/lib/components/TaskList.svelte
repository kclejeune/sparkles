<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { fmtRelative } from '$lib/format';

  let {
    dataset,
    ondone,
    limit = 12,
    refreshKey = 0,
  }: {
    dataset?: string;
    /** Called when a task we saw running finishes. */
    ondone?: (t: api.Task) => void;
    limit?: number;
    /** Bump to force an immediate poll (e.g. right after starting a task). */
    refreshKey?: number;
  } = $props();

  let tasks = $state<api.Task[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let now = $state(Date.now());
  const running = new Set<string>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let alive = true;

  const shown = $derived(tasks.filter((t) => !dataset || t.dataset === dataset).slice(0, limit));

  async function poll() {
    clearTimeout(timer);
    try {
      const list = await api.listTasks();
      list.sort((a, b) => (b.startedAt ?? '').localeCompare(a.startedAt ?? ''));
      for (const t of list) {
        if (t.state === 'running') running.add(t.id);
        else if (running.has(t.id)) {
          running.delete(t.id);
          ondone?.(t);
        }
      }
      tasks = list;
      error = null;
    } catch (e) {
      error = api.errorMessage(e);
    } finally {
      loaded = true;
      now = Date.now();
      const busy = tasks.some((t) => t.state === 'running');
      if (alive) timer = setTimeout(poll, busy ? 700 : 5000);
    }
  }

  $effect(() => {
    void refreshKey;
    poll();
  });

  onMount(() => () => {
    alive = false;
    clearTimeout(timer);
  });
</script>

{#if error}
  <div class="error-box">
    <strong>Could not load tasks.</strong> <span class="muted">{error}</span>
  </div>
{:else if loaded && shown.length === 0}
  <p class="faint none">
    No tasks yet. Compact, backup, reasoning and full-text index jobs show up here.
  </p>
{:else}
  <ul class="tasks">
    {#each shown as t (t.id)}
      <li class={t.state}>
        <span class="state" title={t.state}></span>
        <span class="kind">{t.kind}</span>
        {#if !dataset}<span class="ds mono">{t.dataset}</span>{/if}
        <span class="msg" title={t.message}>
          {#if t.state === 'running'}
            <span class="progress"
              ><span style:width="{Math.round((t.progress ?? 0) * 100)}%"></span></span
            >
            <span class="faint"
              >{t.progress != null ? `${Math.round(t.progress * 100)}%` : 'running'}</span
            >
          {:else}
            {t.message ?? t.state}
          {/if}
        </span>
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
    grid-template-columns: 10px 84px auto 1fr auto;
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
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .kind {
    font-weight: 600;
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
    grid-template-columns: 10px 84px auto 1fr auto;
  }
  li:not(:has(.ds)) {
    grid-template-columns: 10px 84px 1fr auto;
  }
</style>
