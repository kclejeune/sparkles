<script lang="ts">
  // A memory maintenance job of the Settings tab (C18 §8.3 and §8.4): when consolidation or
  // retention last ran and how it ended, when the next scheduled pass is due, and for an
  // admin of the dataset a "Run now" that follows the task. Retention deletes session
  // graphs, so it first runs a dry run and asks with the list of graphs it would delete.
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import {
    KEPT_TEXT,
    nextRunText,
    outcomeText,
    retentionPlan,
    startConsolidation,
    startedText,
    startRetention,
    type MaintenanceEntry,
    type MaintenanceJob,
  } from '$lib/maintenance';
  import * as review from '$lib/review-api';
  import Modal from './Modal.svelte';

  let {
    ds,
    job,
    entry,
    last,
    canAdmin = false,
    readOnly = false,
    onchanged,
  }: {
    ds: string;
    job: MaintenanceJob;
    /** The job in `GET /$/memory/{ds}/maintenance`. */
    entry: MaintenanceEntry | null;
    /** The newest task of the job that the server still knows. */
    last: review.IngestTask | null;
    canAdmin?: boolean;
    /** The server is read-only and refuses maintenance. */
    readOnly?: boolean;
    /** A pass ended: reload the status. */
    onchanged?: () => void;
  } = $props();

  const name = $derived(job === 'consolidation' ? 'Consolidation' : 'Retention');
  /** The task this component started and follows. */
  let task = $state<review.IngestTask | null>(null);
  let busy = $state(false);
  let ctl: AbortController | null = null;
  onDestroy(() => ctl?.abort());

  // the retention dialog: a dry run, then the real pass
  let confirmOpen = $state(false);
  let preview = $state<review.IngestTask | null>(null);
  let previewError = $state<string | null>(null);
  const plan = $derived(preview?.status === 'done' ? retentionPlan(preview.result) : null);

  const shown = $derived(task ?? last);
  const startedAt = $derived(shown?.createdAt ?? entry?.lastRun);
  const next = $derived(nextRunText(entry?.nextRun));
  const pct = $derived(task ? Math.round(task.progress * 100) : 0);

  /** Follow `t` until it ends, calling `update` with each state. */
  async function follow(t: review.IngestTask, update: (t: review.IngestTask) => void) {
    ctl?.abort();
    const c = new AbortController();
    ctl = c;
    let cur = t;
    update(cur);
    while (review.running(cur) && !c.signal.aborted) {
      cur = await review.ingestTask(ds, cur.id, 20, c.signal);
      if (c.signal.aborted) break;
      update(cur);
    }
    return cur;
  }

  async function run(dryRun = false) {
    busy = true;
    try {
      const t =
        job === 'consolidation'
          ? await startConsolidation(ds, { dryRun })
          : await startRetention(ds, { dryRun });
      const end = await follow(t, (x) => (task = x));
      if (end.status === 'failed') toasts.push('error', `${name} failed`, outcomeText(end));
      else if (end.status === 'done') toasts.push('success', `${name} finished`, outcomeText(end));
      onchanged?.();
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError'))
        toasts.error(`${name} did not start`, e);
    } finally {
      busy = false;
    }
  }

  /** Open the retention dialog with a dry run of the pass. */
  async function askRetention() {
    confirmOpen = true;
    preview = null;
    previewError = null;
    try {
      const t = await startRetention(ds, { dryRun: true });
      const end = await follow(t, (x) => (preview = x));
      if (end.status !== 'done') previewError = outcomeText(end);
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError'))
        previewError = api.errorMessage(e);
    }
  }

  function closeDialog() {
    ctl?.abort();
    confirmOpen = false;
  }

  function confirmRetention() {
    confirmOpen = false;
    void run(false);
  }
</script>

<div class="job" data-testid="maintenance-{job}">
  <div class="row line">
    <strong>{name}</strong>
    <span class="status faint">
      {#if task && review.running(task)}
        {task.message ?? 'Running'}…
      {:else if shown}
        Last run {startedText(startedAt)}: {outcomeText(shown)}.
      {:else if entry?.lastRun}
        Last run {startedText(entry.lastRun)}.
      {:else}
        No run recorded.
      {/if}
      {#if next}Next scheduled run {next}.{:else}Not scheduled.{/if}
    </span>
    <span class="spacer"></span>
    {#if canAdmin}
      <button
        class="btn sm"
        disabled={busy ||
          readOnly ||
          (task != null && review.running(task)) ||
          (job === 'retention' && !entry?.settings)}
        title={readOnly
          ? 'The server is read-only'
          : job === 'retention' && !entry?.settings
            ? 'Set “Delete session graphs after” and save before running retention'
            : job === 'consolidation'
              ? 'Start a consolidation pass with the settings above'
              : 'Preview and then delete the session graphs older than the retention'}
        onclick={() => (job === 'retention' ? askRetention() : run())}>Run now</button
      >
    {/if}
  </div>
  {#if task && review.running(task)}
    <div
      class="bar"
      role="progressbar"
      aria-label="{name} progress"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct}
    >
      <span style:width="{pct}%"></span>
    </div>
  {/if}
</div>

{#if job === 'retention'}
  <Modal
    bind:open={confirmOpen}
    title="Delete old session graphs?"
    width={560}
    onclose={closeDialog}
  >
    {#if previewError}
      <div class="error-box"><strong>The preview failed.</strong> {previewError}</div>
    {:else if !plan}
      <p class="faint">
        <span class="spinner"></span> Looking for session graphs older than the retention…
      </p>
    {:else if plan.remove.length === 0}
      <p>
        No session graph is old enough to delete{plan.after ? ` (older than ${plan.after})` : ''},
        so a pass would change nothing.
      </p>
    {:else}
      <p>
        This deletes {plan.remove.length} session graph{plan.remove.length === 1 ? '' : 's'} older than
        {plan.after}, one commit per graph. Deleted graphs stay in the history and in backups.
      </p>
      <ul class="graphs" aria-label="Graphs to delete">
        {#each plan.remove as g (g.graph)}
          <li>
            <span class="mono">{g.graph}</span>
            {#if g.ageDays != null}<span class="faint">{g.ageDays} days old</span>{/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if plan && plan.keep.length}
      <p class="faint small">
        It keeps {plan.keep.length} other graph{plan.keep.length === 1 ? '' : 's'}:
        {Object.entries(
          plan.keep.reduce<Record<string, number>>((a, g) => {
            const why = g.kept ? KEPT_TEXT[g.kept] : 'kept';
            a[why] = (a[why] ?? 0) + 1;
            return a;
          }, {}),
        )
          .map(([why, n]) => `${n} ${why}`)
          .join(', ')}.
      </p>
    {/if}
    {#snippet actions()}
      <button class="btn" onclick={closeDialog}>Cancel</button>
      <button
        class="btn danger solid"
        disabled={!plan || plan.remove.length === 0 || busy}
        onclick={confirmRetention}
        >Delete {plan?.remove.length ?? ''} graph{plan?.remove.length === 1 ? '' : 's'}</button
      >
    {/snippet}
  </Modal>
{/if}

<style>
  .job {
    display: grid;
    gap: 6px;
    margin: 4px 10px 6px;
    padding: 8px 10px;
    border-radius: var(--r);
    background: var(--surface-2);
    font-size: var(--fs-sm);
  }
  .line {
    flex-wrap: wrap;
    gap: 4px 8px;
  }
  .status {
    min-width: 0;
    overflow-wrap: anywhere;
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
  .graphs {
    margin: 8px 0;
    padding: 0 0 0 18px;
    max-height: 220px;
    overflow: auto;
    font-size: var(--fs-sm);
  }
  .graphs li {
    overflow-wrap: anywhere;
  }
  .graphs .faint {
    margin-left: 6px;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>
