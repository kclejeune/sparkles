<script lang="ts">
  import * as api from '$lib/api';
  import * as b from '$lib/backups';
  import { fmtBytes, fmtInt, fmtMs } from '$lib/format';
  import Modal from './Modal.svelte';
  import TaskProgress from './TaskProgress.svelte';

  let {
    target = $bindable(null),
    onfinished,
  }: {
    /** What to verify; null closes the dialog. */
    target?: b.VerifyTarget | null;
    onfinished?: () => void;
  } = $props();

  let level = $state<b.VerifyLevel>('exists');
  let task = $state<api.Task | null>(null);
  let report = $state<b.VerifyReport | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!target) return;
    level = 'exists';
    task = null;
    report = null;
    error = null;
  });

  const LEVELS: { value: b.VerifyLevel; label: string; cost: string }[] = [
    {
      value: 'exists',
      label: 'Exists',
      cost: 'Every referenced blob is present with its size. Listing requests only.',
    },
    {
      value: 'data',
      label: 'Data',
      cost: 'Also downloads and hashes every blob: reads every byte (egress on S3).',
    },
    {
      value: 'restore',
      label: 'Restore',
      cost: 'Also restores into a temporary directory and runs the full integrity check.',
    },
  ];
  const levels = $derived(
    target?.kind === 'repository' ? LEVELS.filter((l) => l.value !== 'restore') : LEVELS,
  );
  const title = $derived(
    target?.kind === 'repository'
      ? `Verify repository ${target.repository}`
      : target
        ? `Verify ${target.backup.name}`
        : 'Verify',
  );

  async function start() {
    if (!target) return;
    busy = true;
    error = null;
    try {
      task =
        target.kind === 'repository'
          ? await b.verifyRepository(target.repository, level === 'data' ? 'data' : 'exists')
          : await b.verifyBackup(...b.pathOf(target.backup), level);
    } catch (e) {
      error = api.errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function finished(t: api.Task) {
    if (t.state === 'done' && t.detail) report = t.detail as b.VerifyReport;
    onfinished?.();
  }

  const failed = $derived(report?.backups.filter((x) => x.status !== 'ok') ?? []);
</script>

<Modal open={target != null} {title} width={520} onclose={() => (target = null)}>
  {#if !task}
    <fieldset class="levels">
      <legend>Depth</legend>
      {#each levels as l (l.value)}
        <label class="opt" class:sel={level === l.value}>
          <input type="radio" bind:group={level} value={l.value} />
          <span><strong>{l.label}</strong><span class="muted">{l.cost}</span></span>
        </label>
      {/each}
    </fieldset>
    {#if target?.kind === 'repository'}
      <p class="faint small">
        Checks every backup in the repository and counts unreferenced blobs (garbage collection
        candidates, not errors).
      </p>
    {/if}
    {#if error}<div class="error-box">{error}</div>{/if}
  {:else}
    {#key task.id}
      <TaskProgress {task} onfinish={finished} />
    {/key}
    {#if report}
      <div class="report">
        <div class="row">
          <span
            class="badge {report.status === 'ok'
              ? 'ok'
              : report.status === 'error'
                ? 'danger'
                : 'warn'}">{report.status}</span
          >
          <span class="muted small"
            >{fmtInt(report.backups.length)} backup{report.backups.length === 1 ? '' : 's'} at level
            {report.level}, {fmtMs(report.millis)}</span
          >
        </div>
        {#if failed.length}
          <ul class="failed">
            {#each failed as f (f.name)}
              <li>
                <strong class="mono">{f.name}</strong>
                {#if f.missing.length}<span
                    >{f.missing.length} missing blob{f.missing.length === 1 ? '' : 's'}</span
                  >{/if}
                {#if f.corrupt.length}<span
                    >{f.corrupt.length} corrupt blob{f.corrupt.length === 1 ? '' : 's'}
                    <span class="mono faint">{f.corrupt[0].slice(0, 16)}…</span></span
                  >{/if}
              </li>
            {/each}
          </ul>
        {/if}
        {#if report.orphans}
          <p class="small muted">
            {fmtInt(report.orphans.blobs)} unreferenced blob{report.orphans.blobs === 1 ? '' : 's'}
            ({fmtBytes(report.orphans.bytes)}): garbage collection removes them after the grace
            period.
          </p>
        {/if}
        <p class="small faint">
          Requests: {fmtInt(report.requests.list)} list, {fmtInt(report.requests.head)} head, {fmtInt(
            report.requests.get,
          )} get.
        </p>
      </div>
    {/if}
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (target = null)}>{task ? 'Close' : 'Cancel'}</button>
    {#if !task}
      <button class="btn primary" onclick={start} disabled={busy}>
        {#if busy}<span class="spinner"></span>{/if} Verify
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .levels {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 6px;
  }
  legend {
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
    margin-bottom: 4px;
    padding: 0;
  }
  .opt {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    cursor: pointer;
  }
  .opt.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
  }
  .opt > span {
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
  }
  .opt input {
    margin-top: 2px;
    accent-color: var(--iri);
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .report {
    display: grid;
    gap: 8px;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .failed {
    margin: 0;
    padding-left: 18px;
    font-size: var(--fs-sm);
    color: var(--danger);
  }
  .failed li span {
    margin-left: 6px;
  }
</style>
