<script lang="ts">
  import * as api from '$lib/api';
  import * as b from '$lib/backups';
  import { fmtBytes, fmtInt, fmtMs, fmtRelative } from '$lib/format';
  import Modal from './Modal.svelte';
  import TaskProgress from './TaskProgress.svelte';

  let {
    repository = $bindable(null),
    onfinished,
  }: {
    /** The repository to collect; null closes the dialog. */
    repository?: b.Repository | null;
    onfinished?: () => void;
  } = $props();

  let graceHours = $state(24);
  /** The dry run, then the real run. */
  let dry = $state<{ task: api.Task; report: b.GcReport | null } | null>(null);
  let real = $state<{ task: api.Task; report: b.GcReport | null } | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!repository) return;
    graceHours = 24;
    dry = null;
    real = null;
    error = null;
  });

  async function start(dryRun: boolean) {
    if (!repository) return;
    busy = true;
    error = null;
    try {
      const task = await b.runGc(repository.name, { dryRun, graceHours });
      if (dryRun) dry = { task, report: null };
      else real = { task, report: null };
    } catch (e) {
      error = api.errorMessage(e);
    } finally {
      busy = false;
    }
  }

  const reportOf = (t: api.Task) =>
    t.state === 'done' && t.detail ? (t.detail as b.GcReport) : null;
</script>

{#snippet summary(r: b.GcReport)}
  <dl class="figures">
    <div>
      <dt>{r.dryRun ? 'Would delete' : 'Deleted'}</dt>
      <dd>{fmtInt(r.deleted)} blob{r.deleted === 1 ? '' : 's'}</dd>
    </div>
    <div>
      <dt>{r.dryRun ? 'Would free' : 'Freed'}</dt>
      <dd>{fmtBytes(r.deletedBytes)}</dd>
    </div>
    <div>
      <dt>Kept (younger than {graceHours} h)</dt>
      <dd>{fmtInt(r.keptYoung)}</dd>
    </div>
    <div>
      <dt>Stored {r.dryRun ? 'afterwards' : 'now'}</dt>
      <dd>{fmtBytes(r.storedBytesAfter)}</dd>
    </div>
  </dl>
  <p class="small faint">
    {fmtInt(r.manifests)} manifests reference {fmtInt(r.referencedBlobs)} of {fmtInt(r.listedBlobs)} blobs
    · {fmtMs(r.millis)}{r.lockWaitMillis ? ` · waited ${fmtMs(r.lockWaitMillis)} for the lock` : ''}
  </p>
{/snippet}

<Modal
  open={repository != null}
  title="Garbage collection: {repository?.name ?? ''}"
  width={540}
  onclose={() => (repository = null)}
>
  <p class="small muted">
    Deletes blobs no backup references. A dry run first shows what would go; the real run holds an
    exclusive repository lock only while it deletes, and backups into this repository wait for it.
  </p>
  {#if repository?.lastGc}
    <p class="small faint">
      Last collected {fmtRelative(repository.lastGc.finished)}: {fmtInt(repository.lastGc.deleted)}
      blobs, {fmtBytes(repository.lastGc.deletedBytes)} freed.
    </p>
  {/if}
  {#if !dry}
    <label class="field">
      Grace period (hours)
      <input class="input" type="number" min="0" bind:value={graceHours} />
      <span class="hint"
        >Unreferenced blobs younger than this stay, for writers that do not take locks.</span
      >
    </label>
  {:else}
    <h3>Dry run</h3>
    {#key dry.task.id}
      <TaskProgress
        task={dry.task}
        onfinish={(t) => {
          if (dry) dry.report = reportOf(t);
        }}
      />
    {/key}
    {#if dry.report}{@render summary(dry.report)}{/if}
  {/if}
  {#if real}
    <h3>Collection</h3>
    {#key real.task.id}
      <TaskProgress
        task={real.task}
        onfinish={(t) => {
          if (real) real.report = reportOf(t);
          onfinished?.();
        }}
      />
    {/key}
    {#if real.report}{@render summary(real.report)}{/if}
  {/if}
  {#if error}<div class="error-box">{error}</div>{/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (repository = null)}>{real ? 'Close' : 'Cancel'}</button>
    {#if !dry}
      <button class="btn primary" onclick={() => start(true)} disabled={busy || !(graceHours >= 0)}>
        {#if busy}<span class="spinner"></span>{/if} Dry run
      </button>
    {:else if !real}
      <button
        class="btn danger solid"
        onclick={() => start(false)}
        disabled={busy || !dry.report || dry.report.deleted === 0}
        title={dry.report?.deleted === 0 ? 'Nothing to delete' : undefined}
      >
        {#if busy}<span class="spinner"></span>{/if}
        Delete {dry.report
          ? `${fmtInt(dry.report.deleted)} blobs (${fmtBytes(dry.report.deletedBytes)})`
          : 'blobs'}
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .hint {
    font-weight: 400;
    color: var(--text-3);
  }
  h3 {
    margin: 4px 0 0;
    font-size: var(--fs);
  }
  .figures {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    margin: 0;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .figures > div {
    padding: 8px 10px;
    border-right: 1px solid var(--border);
  }
  .figures > div:last-child {
    border-right: 0;
  }
  dt {
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  dd {
    margin: 2px 0 0;
    font-weight: 600;
  }
</style>
